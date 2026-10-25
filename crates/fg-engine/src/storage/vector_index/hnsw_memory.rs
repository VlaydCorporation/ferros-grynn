//! Process-wide HNSW memory limits and the shared vector payload cache.

use std::{
	collections::{BTreeMap, BTreeSet},
	sync::{Arc, LazyLock},
};

use parking_lot::Mutex;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Vector,
};

/// Configured default hnsw cache bytes value.
const DEFAULT_HNSW_CACHE_BYTES: usize = 256 * 1024 * 1024;
/// Configured default hnsw build bytes value.
const DEFAULT_HNSW_BUILD_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// Shared hnsw build bytes value.
static HNSW_BUILD_BYTES: LazyLock<Result<usize, String>> =
	LazyLock::new(|| configured_bytes("FG_HNSW_BUILD_MEM", DEFAULT_HNSW_BUILD_BYTES, false));
/// Shared hnsw vector cache value.
static HNSW_VECTOR_CACHE: LazyLock<Result<HnswVectorCache, String>> = LazyLock::new(|| {
	configured_bytes("FG_HNSW_CACHE_SIZE", DEFAULT_HNSW_CACHE_BYTES, true).map(HnswVectorCache::new)
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// Cache key state.
struct CacheKey {
	/// Namespace associated with this cache key.
	namespace: u64,
	/// Ordinal associated with this cache key.
	ordinal: u32,
}

/// Cache entry state.
struct CacheEntry {
	/// Vector associated with this cache entry.
	vector: Arc<Vector>,
	/// Bytes associated with this cache entry.
	bytes: usize,
	/// Stamp associated with this cache entry.
	stamp: u64,
}

#[derive(Default)]
/// Cached HNSW vectors, retained byte count and recency index used for
/// eviction.
struct CacheState {
	/// Bytes associated with this cache state.
	bytes: usize,
	/// Clock associated with this cache state.
	clock: u64,
	/// Entries associated with this cache state.
	entries: BTreeMap<CacheKey, CacheEntry>,
	/// Oldest associated with this cache state.
	oldest: BTreeSet<(u64, CacheKey)>,
}

/// Hnsw vector cache state.
struct HnswVectorCache {
	/// Number of elements the backing allocation can hold.
	capacity: usize,
	// shortcut: one global lock matches the single process-wide budget; shard only if contention
	// is measured.
	/// State associated with this hnsw vector cache.
	state: Mutex<CacheState>,
}

impl HnswVectorCache {
	/// Creates a new `HnswVectorCache` value.
	const fn new(capacity: usize) -> Self {
		Self {
			capacity,
			state: Mutex::new(CacheState {
				bytes: 0,
				clock: 0,
				entries: BTreeMap::new(),
				oldest: BTreeSet::new(),
			}),
		}
	}

	/// Returns hnsw vector cache.
	fn get(&self, key: CacheKey) -> Option<Arc<Vector>> {
		let mut state = self.state.lock();
		let previous = state.entries.get(&key)?.stamp;
		state.oldest.remove(&(previous, key));
		let stamp = next_stamp(&mut state);
		let entry = state.entries.get_mut(&key)?;
		entry.stamp = stamp;
		let vector = Arc::clone(&entry.vector);
		state.oldest.insert((stamp, key));
		Some(vector)
	}

	/// Inserts hnsw vector cache.
	fn insert(&self, key: CacheKey, vector: Arc<Vector>) -> Arc<Vector> {
		let bytes = vector_payload_bytes(&vector);
		if self.capacity == 0 || bytes > self.capacity {
			return vector;
		}
		let mut state = self.state.lock();
		if let Some(existing) = state.entries.get(&key) {
			return Arc::clone(&existing.vector);
		}
		while state.bytes.checked_add(bytes).is_none_or(|total| total > self.capacity) {
			let Some((stamp, oldest)) = state.oldest.pop_first() else {
				break;
			};
			let Some(removed) = state.entries.remove(&oldest) else {
				continue;
			};
			debug_assert_eq!(removed.stamp, stamp);
			state.bytes = state.bytes.saturating_sub(removed.bytes);
		}
		let stamp = next_stamp(&mut state);
		state.bytes += bytes;
		state.oldest.insert((stamp, key));
		state.entries.insert(key, CacheEntry {
			vector: Arc::clone(&vector),
			bytes,
			stamp,
		});
		vector
	}

	/// Performs the purge operation.
	fn purge(&self, namespace: u64) {
		let mut state = self.state.lock();
		let keys = state
			.entries
			.range(
				CacheKey {
					namespace,
					ordinal: 0,
				}..=CacheKey {
					namespace,
					ordinal: u32::MAX,
				},
			)
			.map(|(key, _entry)| *key)
			.collect::<Vec<_>>();
		for key in keys {
			if let Some(entry) = state.entries.remove(&key) {
				state.oldest.remove(&(entry.stamp, key));
				state.bytes = state.bytes.saturating_sub(entry.bytes);
			}
		}
	}
}

/// Returns the next stamp.
fn next_stamp(state: &mut CacheState) -> u64 {
	let Some(next) = state.clock.checked_add(1) else {
		state.entries.clear();
		state.oldest.clear();
		state.bytes = 0;
		state.clock = 1;
		return 1;
	};
	state.clock = next;
	next
}

/// Returns the hnsw build memory bytes result.
pub fn hnsw_build_memory_bytes() -> FerrosGrynnResult<usize> {
	configured(&HNSW_BUILD_BYTES)
}

/// Returns the hnsw vector cache get result.
pub fn hnsw_vector_cache_get(
	namespace: u64,
	ordinal: u32,
) -> FerrosGrynnResult<Option<Arc<Vector>>> {
	Ok(configured_cache()?.get(CacheKey {
		namespace,
		ordinal,
	}))
}

/// Returns the hnsw vector cache insert result.
pub fn hnsw_vector_cache_insert(
	namespace: u64,
	ordinal: u32,
	vector: Arc<Vector>,
) -> FerrosGrynnResult<Arc<Vector>> {
	Ok(configured_cache()?.insert(
		CacheKey {
			namespace,
			ordinal,
		},
		vector,
	))
}

/// Performs the purge hnsw vector cache operation.
pub fn purge_hnsw_vector_cache(namespace: u64) {
	if let Ok(cache) = HNSW_VECTOR_CACHE.as_ref() {
		cache.purge(namespace);
	}
}

/// Returns the configured cache result.
fn configured_cache() -> FerrosGrynnResult<&'static HnswVectorCache> {
	HNSW_VECTOR_CACHE.as_ref().map_err(|reason| FerrosGrynnError::invalid_argument(reason.clone()))
}

/// Returns the configured result.
fn configured(value: &'static LazyLock<Result<usize, String>>) -> FerrosGrynnResult<usize> {
	value.as_ref().copied().map_err(FerrosGrynnError::invalid_argument)
}

/// Returns the configured bytes result.
fn configured_bytes(name: &str, default: usize, allow_zero: bool) -> Result<usize, String> {
	let Some(raw) = std::env::var_os(name) else {
		return Ok(default);
	};
	let raw = raw.into_string().map_err(|_raw| format!("{name} is not valid UTF-8"))?;
	let parsed =
		raw.parse::<u64>().map_err(|error| format!("{name} must be a byte count: {error}"))?;
	let parsed = usize::try_from(parsed).map_err(|_error| format!("{name} exceeds usize"))?;
	if !allow_zero && parsed == 0 {
		return Err(format!("{name} must be greater than zero"));
	}
	Ok(parsed)
}

/// Returns the vector payload bytes result.
fn vector_payload_bytes(vector: &Vector) -> usize {
	let width = fg_runtime::__internal::vector_element_width(vector);
	vector.len().saturating_mul(width)
}

#[cfg(test)]
/// Regression tests for hnsw memory behavior.
mod tests {
	use std::sync::Arc;

	use super::{CacheKey, HnswVectorCache};
	use crate::values::Vector;

	/// Verifies hnsw vector cache enforces one budget across namespaces.
	#[test]
	fn hnsw_vector_cache_enforces_one_budget_across_namespaces() {
		let cache = HnswVectorCache::new(8);
		let first = CacheKey {
			namespace: 1,
			ordinal: 0,
		};
		let second = CacheKey {
			namespace: 2,
			ordinal: 0,
		};
		cache.insert(first, Arc::new(Vector::from(vec![1.0_f32, 2.0])));
		assert!(cache.get(first).is_some());
		cache.insert(second, Arc::new(Vector::from(vec![3.0_f32, 4.0])));

		assert!(cache.get(first).is_none());
		assert!(cache.get(second).is_some());
		assert_eq!(cache.state.lock().bytes, 8);
	}

	/// Verifies hnsw vector cache does not retain oversized vectors.
	#[test]
	fn hnsw_vector_cache_does_not_retain_oversized_vectors() {
		let cache = HnswVectorCache::new(4);
		let key = CacheKey {
			namespace: 1,
			ordinal: 0,
		};
		let vector = Arc::new(Vector::from(vec![1.0_f64]));
		let returned = cache.insert(key, Arc::clone(&vector));

		assert!(Arc::ptr_eq(&returned, &vector));
		assert!(cache.get(key).is_none());
	}
}
