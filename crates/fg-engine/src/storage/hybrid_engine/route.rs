//! Pure physical-read routing for the hybrid storage engine.

use crate::storage::PageIoMode;

/// Maximum number of logical page operations protected by one read epoch.
pub const MAX_READ_SCOPE_OPERATIONS: usize = 256;
/// Maximum number of pages submitted by one scatter extent.
pub const MAX_SCATTER_PAGES: usize = 128;
/// Maximum frame-oriented IoRing queue depth.
pub const MAX_IORING_QUEUE_DEPTH: usize = 32;
/// Fixed sequential prefetch depth selected by the prototype measurements.
#[cfg(feature = "storage-engine-prototype-api")]
pub const PREFETCH_DEPTH: usize = 1;

/// Physical intent inferred by a storage cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadIntent {
	/// One unpredictable page read.
	#[cfg(feature = "storage-engine-prototype-api")]
	Point,
	/// Index or graph traversal whose hot pages should remain resident.
	IndexTraversal,
	/// Ordered scan whose cold pages should bypass resident replacement.
	#[cfg(feature = "storage-engine-prototype-api")]
	SequentialScan,
	/// One-pass bulk read or build input.
	#[cfg(feature = "storage-engine-prototype-api")]
	BulkBuild,
}

/// Bounded page window known before transport submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadWindow {
	/// One physical page.
	#[cfg(feature = "storage-engine-prototype-api")]
	Single,
	/// Consecutive home-file pages.
	#[cfg(feature = "storage-engine-prototype-api")]
	Contiguous {
		/// Number of requested pages.
		pages: usize,
	},
	/// Known page numbers that are not contiguous.
	Sparse {
		/// Total logical operations in the protected window.
		operations: usize,
		/// Cold pages remaining after resident probes.
		cold_pages: usize,
	},
}

/// Platform transports available before an operation is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransportCapabilities {
	/// Windows `ReadFileScatter` is available.
	#[cfg(feature = "storage-engine-prototype-api")]
	pub scatter: bool,
	/// Windows IoRing is available and enabled at build time.
	pub io_ring: bool,
}

impl TransportCapabilities {
	/// Selects independently available scatter and native-ring transports.
	#[cfg(feature = "storage-engine-prototype-api")]
	pub const fn new(scatter: bool, io_ring: bool) -> Self {
		Self {
			scatter,
			io_ring,
		}
	}

	/// Returns capabilities compiled for the current target.
	pub const fn current() -> Self {
		Self {
			#[cfg(feature = "storage-engine-prototype-api")]
			scatter: cfg!(windows),
			io_ring: cfg!(all(windows, feature = "native-async-io")),
		}
	}
}

/// Concrete path selected before any new I/O starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadRoute {
	/// Exact immutable page version is already resident in R8.
	AtomicArena,
	/// Synchronous validated Direct I/O.
	SyncDirect {
		/// Whether a validated miss enters the resident arena.
		admit: bool,
	},
	/// Cache-bypassing scatter pipeline.
	#[cfg(feature = "storage-engine-prototype-api")]
	ScatterBypass {
		/// Pages read by one of the two reusable buffers.
		extent_pages: usize,
		/// Number of extents read ahead.
		prefetch_depth: usize,
	},
	/// Cache-bypassing frame-oriented IoRing batch.
	IoRingSparse {
		/// Number of cold frames submitted together.
		queue_depth: usize,
	},
}

/// Returns the bounded transient-frame budget for a resident pool.
pub const fn transient_pages(buffer_pool_pages: usize) -> usize {
	let ten_percent = buffer_pool_pages / 10;
	if ten_percent < 288 {
		ten_percent
	} else {
		288
	}
}

/// Returns the fixed sparse-window share of the transient budget.
pub const fn sparse_transient_pages(transient_budget: usize) -> usize {
	let share = transient_budget / 9;
	if share < MAX_IORING_QUEUE_DEPTH {
		share
	} else {
		MAX_IORING_QUEUE_DEPTH
	}
}

/// Returns the page count of either reusable scatter buffer.
pub const fn scatter_extent_pages(transient_budget: usize) -> usize {
	let available = transient_budget.saturating_sub(sparse_transient_pages(transient_budget)) / 2;
	if available < MAX_SCATTER_PAGES {
		available
	} else {
		MAX_SCATTER_PAGES
	}
}

/// Selects one read path without starting I/O or mutating cache state.
pub const fn route_read(
	exact_version_resident: bool,
	intent: ReadIntent,
	window: ReadWindow,
	io_mode: PageIoMode,
	capabilities: TransportCapabilities,
	transient_budget: usize,
) -> ReadRoute {
	if exact_version_resident {
		return ReadRoute::AtomicArena;
	}

	#[cfg(feature = "storage-engine-prototype-api")]
	let ReadWindow::Sparse {
		operations,
		cold_pages,
	} = window
	else {
		if matches!(intent, ReadIntent::SequentialScan | ReadIntent::BulkBuild) {
			if let ReadWindow::Contiguous {
				pages,
			} = window
			{
				let extent_pages =
					min3(pages, scatter_extent_pages(transient_budget), MAX_SCATTER_PAGES);
				if pages > 1 && extent_pages > 1 && capabilities.scatter {
					return ReadRoute::ScatterBypass {
						extent_pages,
						prefetch_depth: PREFETCH_DEPTH,
					};
				}
			}
			return ReadRoute::SyncDirect {
				admit: false,
			};
		}
		return ReadRoute::SyncDirect {
			admit: matches!(window, ReadWindow::Single),
		};
	};
	#[cfg(not(feature = "storage-engine-prototype-api"))]
	let ReadWindow::Sparse {
		operations,
		cold_pages,
	} = window;

	let queue_depth =
		min3(cold_pages, sparse_transient_pages(transient_budget), MAX_IORING_QUEUE_DEPTH);
	if operations <= MAX_READ_SCOPE_OPERATIONS
		&& cold_pages > 1
		&& queue_depth > 1
		&& !matches!(io_mode, PageIoMode::Sync)
		&& capabilities.io_ring
	{
		return ReadRoute::IoRingSparse {
			queue_depth,
		};
	}
	ReadRoute::SyncDirect {
		admit: matches!(intent, ReadIntent::IndexTraversal),
	}
}

/// Returns the least of three values in const contexts.
const fn min3(left: usize, middle: usize, right: usize) -> usize {
	let pair = if left < middle {
		left
	} else {
		middle
	};
	if pair < right {
		pair
	} else {
		right
	}
}

#[cfg(test)]
/// Regression tests for route behavior.
mod tests {
	use super::*;

	/// Verifies sparse sync index traversal admits validated pages.
	#[test]
	fn sparse_sync_index_traversal_admits_validated_pages() {
		let route = route_read(
			false,
			ReadIntent::IndexTraversal,
			ReadWindow::Sparse {
				operations: 8,
				cold_pages: 8,
			},
			PageIoMode::Sync,
			TransportCapabilities {
				#[cfg(feature = "storage-engine-prototype-api")]
				scatter: false,
				io_ring: false,
			},
			288,
		);
		assert_eq!(route, ReadRoute::SyncDirect {
			admit: true
		});
	}
}
