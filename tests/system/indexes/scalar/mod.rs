/// Public scalar index lookup tests using the in-memory backend.
mod in_memory;
#[cfg(windows)]
/// Public scalar index lookup tests using synchronous local storage.
mod local;
#[cfg(all(windows, feature = "native-async-io"))]
/// Public scalar index lookup tests using native asynchronous local I/O.
mod local_native_async;
