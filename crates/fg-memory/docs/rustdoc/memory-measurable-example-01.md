```
# extern crate fg_memory as ferros_grynn;
use ferros_grynn::{
	HeapEstimatorCache,
	Measurable,
	NO_HEAP_ESTIMATOR_CACHE_INSTANCE,
	NoHeapEstimatorCache,
};

struct InlineValue;
ferros_grynn::impl_zero_heap!(InlineValue);

let value = InlineValue;
assert_eq!(value.estimated_heap_usage(), 0);
assert_eq!(value.estimated_heap_usage_cache(&NO_HEAP_ESTIMATOR_CACHE_INSTANCE), 0);
let mut cache = NoHeapEstimatorCache::new_with_same_settings();
cache.reset();
assert_eq!(cache.estimated_heap_usage(&value, 0), 0);
```
