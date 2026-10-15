/// Public transaction isolation and publication tests using the in-memory
/// backend.
mod in_memory;
#[cfg(windows)]
/// Public transaction isolation and publication tests using synchronous local
/// storage.
mod local;
#[cfg(all(windows, feature = "native-async-io"))]
/// Public transaction isolation and publication tests using native asynchronous
/// local I/O.
mod local_native_async;
