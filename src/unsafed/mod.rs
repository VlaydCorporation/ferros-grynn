/// Prototype errors for low-level memory operations.
mod error;
/// Prototype shallow-size, alignment and heap-size estimation helpers.
mod heap_estimator;
/// Unsafe allocation and raw-memory helpers with caller-owned validity checks.
mod memory;
/// Prototype process and operating-system resource observations.
mod os;

pub use heap_estimator::*;
