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

const DEFAULT_HNSW_CACHE_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_HNSW_BUILD_BYTES: usize = 2 * 1024 * 1024 * 1024;

static HNSW_BUILD_BYTES: LazyLock<Result<usize, String>> =
	LazyLock::new(|| configured_bytes("FG_HNSW_BUILD_MEM", DEFAULT_HNSW_BUILD_BYTES, false));
static HNSW_VECTOR_CACHE: LazyLock<Result<HnswVectorCache, String>> = LazyLock::new(|| {
	configured_bytes("FG_HNSW_CACHE_SIZE", DEFAULT_HNSW_CACHE_BYTES, true).map(HnswVectorCache::new)
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
	namespace: u64,
	ordinal: u32,
}

struct CacheEntry {
	vector: Arc<Vector>,
	bytes: usize,
	stamp: u64,
}

#[derive(Default)]
struct CacheState {
	bytes: usize,
	clock: u64,
	entries: BTreeMap<CacheKey, CacheEntry>,
	oldest: BTreeSet<(u64, CacheKey)>,
}

struct HnswVectorCache {
	capacity: usize,
	// ponytail: one global lock matches the single process-wide budget; shard only if contention
	// is measured.
	state: Mutex<CacheState>,
}

impl HnswVectorCache {
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

pub(crate) fn hnsw_build_memory_bytes() -> FerrosGrynnResult<usize> {
	configured(&HNSW_BUILD_BYTES)
}

pub(crate) fn hnsw_vector_cache_get(
	namespace: u64,
	ordinal: u32,
) -> FerrosGrynnResult<Option<Arc<Vector>>> {
	Ok(configured_cache()?.get(CacheKey {
		namespace,
		ordinal,
	}))
}

pub(crate) fn hnsw_vector_cache_insert(
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

pub(crate) fn purge_hnsw_vector_cache(namespace: u64) {
	if let Ok(cache) = HNSW_VECTOR_CACHE.as_ref() {
		cache.purge(namespace);
	}
}

fn configured_cache() -> FerrosGrynnResult<&'static HnswVectorCache> {
	HNSW_VECTOR_CACHE.as_ref().map_err(|reason| FerrosGrynnError::invalid_argument(reason.clone()))
}

fn configured(value: &'static LazyLock<Result<usize, String>>) -> FerrosGrynnResult<usize> {
	value.as_ref().copied().map_err(FerrosGrynnError::invalid_argument)
}

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

fn vector_payload_bytes(vector: &Vector) -> usize {
	let width = match vector {
		Vector::F64(_) | Vector::I64(_) => 8,
		Vector::F32(_) | Vector::I32(_) => 4,
		Vector::F16(_) | Vector::I16(_) => 2,
		Vector::I8(_) | Vector::U8(_) => 1,
	};
	vector.len().saturating_mul(width)
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::{CacheKey, HnswVectorCache};
	use crate::values::Vector;

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
