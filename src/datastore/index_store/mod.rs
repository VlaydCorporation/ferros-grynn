//! MVCC-owned engine and user indexes over normalized atoms and graph
//! occurrences.

mod model;
mod mutation;
mod rebuild;

pub(crate) use model::{IndexBinding, IndexEntries, IndexStoreRoot};
pub(crate) use mutation::index_context;
pub(crate) use rebuild::rebuild_index_store;

#[cfg(test)]
mod tests;
