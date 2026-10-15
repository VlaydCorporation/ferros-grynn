/// Public HNSW vector indexing and search tests using the in-memory backend.
mod in_memory;
#[cfg(windows)]
/// Public HNSW vector indexing and search tests using synchronous local
/// storage.
mod local;
#[cfg(all(windows, feature = "native-async-io"))]
/// Public HNSW vector indexing and search tests using native asynchronous local
/// I/O.
mod local_native_async;
