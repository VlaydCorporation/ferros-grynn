//! Crate-private local storage primitives.
//!
//! The current slice contains synchronous Direct I/O and a read-only buffer
//! pool. WAL, MVCC, and optional native async I/O remain later stages.

mod buffer_pool;
mod direct;
mod page;
mod page_manager;

#[cfg(test)]
mod benchmarks;
