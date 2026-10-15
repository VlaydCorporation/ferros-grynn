/// Public public SDK contracts tests using the in-memory backend.
mod in_memory;
#[cfg(windows)]
/// Public public SDK contracts tests using synchronous local storage.
mod local;
#[cfg(all(windows, feature = "native-async-io"))]
/// Public public SDK contracts tests using native asynchronous local I/O.
mod local_native_async;
