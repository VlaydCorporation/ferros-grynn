//! MVCC-owned engine and user indexes over normalized atoms and graph
//! occurrences.

/// Shared key construction and publication-only preparation admission.
mod key_preparation;
/// Logical index bindings and entry representations.
mod model;
/// Transactional preparation and application of index changes.
mod mutation;
mod rebuild;

pub use model::{IndexBinding, IndexEntries, IndexStoreRoot};
pub use mutation::{IndexAddress, PreparedIndexChange, index_context};
pub use rebuild::{rebuild_index_store, rebuild_lookup_index_store};

#[cfg(test)]
/// Regression tests for index store behavior.
mod tests;
