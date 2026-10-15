/// Public columnar presence and value conversion tests using the in-memory
/// backend.
mod in_memory;
#[cfg(windows)]
/// Public columnar presence and value conversion tests using synchronous local
/// storage.
mod local;
#[cfg(all(windows, feature = "native-async-io"))]
/// Public columnar presence and value conversion tests using native
/// asynchronous local I/O.
mod local_native_async;
