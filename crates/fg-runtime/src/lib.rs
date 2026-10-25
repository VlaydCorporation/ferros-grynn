//! Owned graph/value runtime, independent of persistence, syntax and Arrow.
/// Reusable snapshot copy-on-write vectors.
mod collections;
pub mod ecs;
pub mod graph;
pub mod math;
pub mod values;
use fg_gql_status as gql_status;
use fg_memory as memory;
/// Workspace-only runtime contracts, never re-exported by the facade.
#[doc(hidden)]
#[path = "internal/mod.rs"]
pub mod __internal;
#[cfg(test)]
pub use fg_test_support as test_assertions;
