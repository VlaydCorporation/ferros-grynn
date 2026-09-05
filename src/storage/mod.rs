//! Crate-private local storage primitives.
//!
//! The current slice contains the hybrid resident/direct read engine,
//! database-level frame WAL, sealed file routing, checkpoint/recovery, concrete
//! graph pages, and transaction-bound streaming views. The Windows-first local
//! datastore uses validated scatter/prefetch for contiguous scans and optional
//! frame-oriented IoRing for known sparse windows.

#[cfg(any(test, feature = "storage-engine-prototype-api"))]
mod buffer_pool;
mod catalog;
mod consts;
mod direct_io;
mod file_registry;
mod graph_index;
mod graph_pages;
mod hybrid_engine;
mod local;
mod page;
mod page_cache;
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
		pub use super::super::{
			buffer_pool::{BufferPool, PageGuard},
			page_cache::{PageCacheKey, PageCacheVersion, validate_loaded_header},
		};
	}

	pub mod direct {
		pub use super::super::direct_io::{DirectFile, DirectFileOpenMode};
	}

	/// Measurement-only access used by the external storage prototype crate.
	pub mod hybrid {
		#[cfg(all(windows, feature = "native-async-io"))]
		pub use super::super::hybrid_engine::SparseReadBatch;
		pub use super::super::hybrid_engine::{
			AtomicArenaBufferPool,
			AtomicBatchReservation,
			AtomicEpochPage,
			AtomicPageGuard,
			HybridReadEngine,
			MAX_IORING_QUEUE_DEPTH,
			MAX_READ_SCOPE_OPERATIONS,
			MAX_SCATTER_PAGES,
			PREFETCH_DEPTH,
			ReadIntent,
			ReadRoute,
			ReadScope,
			ReadWindow,
			ResolvedFrameHandle,
			ResolvedPageTable,
			ResolvedPageWindow,
			SourceBoundLocator,
			TransportCapabilities,
			route_read,
			scatter_extent_pages,
			sparse_transient_pages,
			transient_pages,
		};
		#[cfg(windows)]
		pub use super::super::hybrid_engine::{ScatterPrefetchCursor, ValidatedScatterExtent};
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
		pub use super::super::page_io::{FilePageExtent, PageIoMode};
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
pub(crate) use store::LocalTableCursor;
pub use store::{
	LocalCommitGraph,
	LocalCommitGraphChange,
	LocalCommitNode,
	LocalCommitResult,
	LocalCommitTable,
	LocalCommitTableField,
	LocalCommitTableIndex,
	LocalDatabaseSnapshot,
	LocalEdgeRead,
	LocalGraph,
	LocalGraphView,
	LocalNodeRead,
	LocalReadTransaction,
	LocalStore,
};

#[cfg(test)]
mod benchmarks;
