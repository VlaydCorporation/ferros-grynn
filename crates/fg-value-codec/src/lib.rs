//! Canonical value DTOs, budgets and codecs for Ferros-Grynn.
mod values;
use fg_gql_status as gql_status;
use fg_memory as memory;
#[cfg(test)]
use fg_runtime::graph;
/// Shared assertions available only to the owner test harness.
#[cfg(test)]
pub use fg_test_support as test_assertions;
/// Workspace-only DTO and codec contracts, excluded from the public SDK.
#[doc(hidden)]
pub use values::storage as __internal;
