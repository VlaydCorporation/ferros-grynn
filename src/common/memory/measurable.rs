use crate::common::memory::HeapEstimatorCache;
use crate::unsafed::shallow_size_of;
pub use ferros_grynn_macros::Measurable;
use std::collections::HashMap;

pub trait Measurable {
	/// Gives an estimation of the heap usage in bytes for the given value.
	fn estimated_heap_usage(&self) -> usize;

	/// Gives an estimation of the heap usage in bytes for the given value,
	/// via the given `HeapEstimatorCache` which could have the ability to cache
	/// and de-duplicate estimations.
	fn estimated_heap_usage_cache(&self, estimator_cache: &impl HeapEstimatorCache) -> usize
	where
		Self: Sized,
	{
		let estimate = self.estimated_heap_usage();
		estimator_cache.estimated_heap_usage(self, estimate)
	}
}

#[macro_export]
macro_rules! impl_zero_heap {
    ($($ident:ident),+) => {
		$(
			impl crate::common::memory::Measurable for $ident {
				fn estimated_heap_usage(&self) -> usize { 0 }
			}
		)*
	};
}

pub fn size_of_hash_map<K, V>(map: &HashMap<K, V>) -> usize {
	size_of_val(map) + map.capacity() * shallow_size_of::<(K, V)>()
}
