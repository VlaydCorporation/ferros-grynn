//! Crate-private local storage primitives.
//!
//! The current slice contains synchronous Direct I/O, a fixed buffer pool,
//! database-level frame WAL, sealed file routing, checkpoint/recovery, concrete
//! graph pages, and lazy transaction-bound graph views. The Windows-first local
//! datastore composes these primitives with durable graph generations; optional
//! native async I/O remains a later slice.

mod buffer_pool;
mod catalog;
mod consts;
mod direct_io;
mod file_registry;
mod graph_index;
mod graph_pages;
mod local;
mod page;
mod page_io;
mod page_manager;
mod store;
mod superblock;
mod types;
mod wal;

#[cfg(feature = "storage-engine-prototype-api")]
#[doc(hidden)]
pub mod prototype_api {
	/// Builds a prototype storage error while retaining its native source.
	pub fn storage_io_failure_with_source<E>(
		operation: impl Into<String>,
		source: E,
	) -> crate::gql_status::FerrosGrynnError
	where
		E: std::error::Error + Send + Sync + 'static,
	{
		crate::gql_status::FerrosGrynnError::storage_io_failure(operation)
			.with_technical_source(source)
	}

	pub mod buffer_pool {
		pub use super::super::buffer_pool::{
			BufferPool,
			PageCacheKey,
			PageCacheVersion,
			PageGuard,
			validate_loaded_header,
		};
	}

	pub mod direct {
		pub use super::super::direct_io::{DirectFile, DirectFileOpenMode};
	}

	pub mod page {
		pub use super::super::page::{
			AlignedPage,
			PAGE_HEADER_SIZE,
			PAGE_SIZE,
			PageFlags,
			PageHeader,
			PageLsn,
			PageNo,
			PageType,
			validate_page_bytes,
		};
	}

	pub mod page_io {
		pub use super::super::page_io::FilePageExtent;
		#[cfg(all(windows, feature = "native-async-io"))]
		pub use super::super::page_io::{FrameRead, IoRingSubmissionStats, WindowsIoRing};
	}

	pub mod types {
		pub use super::super::types::{
			CheckpointGeneration,
			DataFileId,
			DataFileKind,
			GraphId,
			Lsn,
			PageKey,
		};
	}
}

// Preserve the internal module path while callers migrate to `direct_io`.
use direct_io as direct;
pub use page_io::PageIoMode;
pub use store::{
	LocalCommitGraph,
	LocalCommitResult,
	LocalCommitTable,
	LocalCommitTableField,
	LocalCommitTableIndex,
	LocalDatabaseSnapshot,
	LocalEdgeRead,
	LocalGraph,
	LocalGraphView,
	LocalNodeRead,
	LocalQueryAtom,
	LocalReadTransaction,
	LocalStore,
};

#[cfg(test)]
mod benchmarks;
