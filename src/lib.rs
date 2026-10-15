// mod common;
/// Owned database adapters and durable correspondence references.
pub mod bridge;
pub mod changefeed;
/// Reusable containers for snapshots, registries and ordered lookup.
mod collections;
pub mod datastore;
pub mod ecs;
/// GrynnQL semantic analysis, logical planning and query execution.
pub mod gql;
pub mod graph;
pub mod live;
/// Checked arithmetic and mathematical helpers shared across project
/// subsystems.
pub mod math;
/// Allocator selection, heap estimates and memory accounting.
mod memory;
pub use memory::{
	HeapEstimatorCache,
	Measurable,
	NO_HEAP_ESTIMATOR_CACHE_INSTANCE,
	NoHeapEstimatorCache,
};
pub mod stats;
mod storage;
/// GrynnQL tokens, syntax trees and parser entry points.
pub mod syn;
#[cfg(feature = "storage-engine-prototype-api")]
#[doc(hidden)]
pub use storage::prototype_api as storage_engine_prototype_api;
/// Owned runtime values, explicit storage representations and columnar
/// adapters.
pub mod values;
// mod asynchronous;

/// Shared assertions available only in the unit-test harness.
#[cfg(test)]
#[path = "../tests/common/assertions.rs"]
pub mod test_assertions;

/// Project-wide structured statuses, diagnostics, outcomes and errors.
pub mod gql_status {
	pub use fg_gql_status::*;
}
