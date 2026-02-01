use crate::common::memory::Measurable;

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
