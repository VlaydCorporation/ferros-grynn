/// Public public schema definitions tests using the in-memory backend.
mod in_memory;
#[cfg(windows)]
/// Public public schema definitions tests using synchronous local storage.
mod local;
#[cfg(all(windows, feature = "native-async-io"))]
/// Public public schema definitions tests using native asynchronous local I/O.
mod local_native_async;
