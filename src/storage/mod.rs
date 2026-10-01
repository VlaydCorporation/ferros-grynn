//! Candidate-v1 local storage for the normalized AtomStore and graph
//! occurrences.

mod atom_page;
mod atom_store_image;
mod atom_store_superblock;
mod base_statistics;
#[cfg(feature = "storage-engine-prototype-api")]
mod buffer_pool;
mod catalog;
mod consts;
mod database_superblock;
mod direct_io;
mod file_registry;
mod fulltext_index;
mod fulltext_index_delta;
mod graph_index;
mod graph_pages;
mod graph_store;
mod graph_superblock;
mod hybrid_engine;
mod image_io;
mod immutable_stream;
mod job_catalog;
mod local;
mod local_store;
mod membership_index;
mod metadata_cow;
mod page;
mod page_cache;
mod page_io;
mod property_dictionary;
mod property_page;
mod quarantine;
mod scoped_index_image;
mod types;
mod vector_index;
mod wal;

use direct_io as direct;
pub(crate) use fulltext_index_delta::FulltextDeltaRecord;
pub(crate) use local_store::{
	LocalQuerySnapshot,
	LocalStore,
	PhysicalIndexAtom,
	PhysicalIndexDelta,
	PhysicalIndexEntries,
	PhysicalIndexRead,
	PhysicalScalarGroup,
	PhysicalTraversalFieldsRead,
	PhysicalTraversalInnerRead,
	PhysicalTraversalRead,
};
pub use page_io::PageIoMode;
pub(crate) use quarantine::{QuarantineArchiveReader, QuarantineArtifact, QuarantineWriter};
pub(crate) use vector_index::VectorSearchHit;

#[cfg(feature = "storage-engine-prototype-api")]
#[doc(hidden)]
pub mod prototype_api {
	/// Clears resident frames before an isolated prototype measurement.
	///
	/// This non-default measurement boundary is not a supported datastore API.
	///
	/// # Errors
	///
	/// Returns an arena lifecycle error if a resident mapping changes while it
	/// is being removed.
	pub fn clear_resident_pages(
		store: &crate::datastore::Datastore,
	) -> crate::gql_status::FerrosGrynnResult<usize> {
		store.inner.clear_resident_for_prototype()
	}

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
			Lsn,
			PageKey,
		};
		pub use crate::graph::GraphId;
	}
}
pub(crate) use graph_index::{
	ExternalIndexSorter,
	SortedIndexRecords,
	check_external_sort_cancelled,
	external_sort_metrics,
	external_sort_run_budget,
	with_external_sort_control,
	with_external_sort_options,
};

/// Returns the canonical number of global atom hot pages for one slot ceiling.
pub(crate) const fn atom_hot_page_count(slot_capacity: usize) -> usize {
	slot_capacity.div_ceil(atom_page::ATOM_SLOTS_PER_PAGE as usize)
}

/// Returns the canonical number of graph-local node and edge hot pages.
pub(crate) const fn graph_hot_page_count(node_capacity: usize, edge_capacity: usize) -> usize {
	node_capacity
		.div_ceil(graph_pages::HOT_SLOTS_PER_PAGE)
		.saturating_add(edge_capacity.div_ceil(graph_pages::HOT_SLOTS_PER_PAGE))
}
