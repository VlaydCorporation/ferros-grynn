//! Crate-private local storage primitives.
//!
//! The current slice contains synchronous Direct I/O, a fixed buffer pool,
//! database-level frame WAL, sealed file routing, passive checkpoint, and
//! recovery. Public local-database composition and optional native async I/O
//! remain later stages.

mod buffer_pool;
mod direct;
mod file_registry;
mod local;
mod page;
mod page_manager;
mod superblock;
mod types;
mod wal;

#[cfg(test)]
mod benchmarks;
