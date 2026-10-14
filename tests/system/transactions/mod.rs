mod in_memory;
#[cfg(windows)]
mod local;
#[cfg(all(windows, feature = "native-async-io"))]
mod local_native_async;
