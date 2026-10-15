/// Operations required for measurable.
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

/// Implements [`Measurable`] for types that own no heap
/// allocations.
///
/// Each listed type must keep all of its data inline. The generated estimate
/// excludes the value's own stack or enclosing allocation.
#[doc = include_str!("../../docs/rustdoc/memory-measurable-example-01.md")]
#[macro_export]
macro_rules! impl_zero_heap {
    ($($ident:ident),+) => {
		$(
			impl $crate::Measurable for $ident {
				/// Returns the heap allocation estimate generated for this type.
				fn estimated_heap_usage(&self) -> usize { 0 }
			}
		)*
	};
}

/// Operations required for heap estimator cache.
pub trait HeapEstimatorCache {
	/// Returns the new with same settings result.
	fn new_with_same_settings() -> Self;
	/// Returns the estimated heap usage result.
	fn estimated_heap_usage(&self, measurable: &impl Measurable, estimate: usize) -> usize;
	/// Resets heap estimator cache.
	fn reset(&mut self);
}

/// No heap estimator cache state.
#[non_exhaustive]
pub struct NoHeapEstimatorCache {}

impl HeapEstimatorCache for NoHeapEstimatorCache {
	/// Returns new with same settings for `NoHeapEstimatorCache`.
	fn new_with_same_settings() -> Self {
		Self {}
	}

	/// Returns estimated heap usage for `NoHeapEstimatorCache`.
	fn estimated_heap_usage(&self, _measurable: &impl Measurable, estimate: usize) -> usize {
		estimate
	}

	/// Performs the reset operation for `NoHeapEstimatorCache`.
	fn reset(&mut self) {}
}

/// Configured no heap estimator cache instance value.
pub const NO_HEAP_ESTIMATOR_CACHE_INSTANCE: NoHeapEstimatorCache = NoHeapEstimatorCache {};
