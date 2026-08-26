//! Crate-private local storage primitives.
//!
//! The current slice contains synchronous Direct I/O, a fixed buffer pool,
//! database-level frame WAL, sealed file routing, checkpoint/recovery, concrete
//! graph pages, and lazy transaction-bound graph views. The Windows-first local
//! datastore composes these primitives with durable graph generations; optional
//! native async I/O remains a later slice.

mod buffer_pool;
mod catalog;
mod direct;
mod file_registry;
mod graph_index;
mod graph_pages;
mod local;
mod page;
mod page_manager;
mod store;
mod superblock;
mod types;
mod wal;

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
