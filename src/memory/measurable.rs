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

pub trait HeapEstimatorCache {
	fn new_with_same_settings() -> Self;
	fn estimated_heap_usage(&self, measurable: &impl Measurable, estimate: usize) -> usize;
	fn reset(&mut self);
}

struct NoHeapEstimatorCache {}

impl HeapEstimatorCache for NoHeapEstimatorCache {
	fn new_with_same_settings() -> Self {
		Self {}
	}

	fn estimated_heap_usage(&self, _measurable: &impl Measurable, estimate: usize) -> usize {
		estimate
	}

	fn reset(&mut self) {}
}

pub const NO_HEAP_ESTIMATOR_CACHE_INSTANCE: NoHeapEstimatorCache = NoHeapEstimatorCache {};
