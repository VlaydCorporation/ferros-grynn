use std::sync::Weak;

use parking_lot::RwLock;
use rapidhash::{HashMapExt, RapidHashMap};

/// Shared memory reporters value.
static MEMORY_REPORTERS: RwLock<Vec<MemoryReporterEntry>> = RwLock::new(Vec::new());

/// Memory reporter entry state.
struct MemoryReporterEntry {
	/// The name of the reporter
	pub name: String,
	/// The weak reference to the reporter
	pub reporter: Weak<dyn MemoryReporter>,
}

/// Trait for objects that can report their memory usage to the global allocator
/// tracker
pub trait MemoryReporter: Send + Sync {
	/// Returns the amount of memory currently allocated by this object
	fn memory_allocated(&self) -> usize;
}

/// Returns the total memory allocated by all separate memory reporters
pub fn memory_reporters_allocated_total() -> usize {
	// Acquire the read lock
	let reporters = MEMORY_REPORTERS.read();
	// Get the total memory allocated
	reporters
		.iter()
		.filter_map(|r| r.reporter.upgrade())
		.map(|reporter| reporter.memory_allocated())
		.sum()
}

/// Returns the memory allocated by each memory reporter by name
pub fn memory_reporters_allocated_by_name() -> RapidHashMap<String, usize> {
	// Acquire the read lock
	let reporters = MEMORY_REPORTERS.read();
	// Create a new HashMap to store the memory allocated by name
	let mut output = RapidHashMap::new();
	// Iterate over the reporters
	for r in reporters.iter() {
		output.insert(
			r.name.clone(),
			r.reporter.upgrade().map(|v| v.memory_allocated()).unwrap_or(0),
		);
	}
	// Return the HashMap
	output
}

/// Registers a new memory reporter with the global allocator tracker
pub fn register_memory_reporter(name: &str, reporter: Weak<dyn MemoryReporter>) {
	// Convert the name to a string
	let name = name.to_string();
	// Acquire the write lock
	let mut reporters = MEMORY_REPORTERS.write();
	// Clean up dead weak references while we're here
	reporters.retain(|r| r.reporter.strong_count() > 0);
	// Add the reporter to the list
	reporters.push(MemoryReporterEntry {
		name,
		reporter,
	});
}

/// Cleans up dead weak references from the memory reporters list
pub fn cleanup_memory_reporters() {
	// Acquire the write lock
	let mut reporters = MEMORY_REPORTERS.write();
	// Clean up dead weak references while we're here
	reporters.retain(|r| r.reporter.strong_count() > 0);
}

/// The registry must observe reporters without extending their ownership.
#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::super::{
		MemoryReporter,
		cleanup_memory_reporters,
		memory_reporters_allocated_by_name,
		memory_reporters_allocated_total,
		register_memory_reporter,
	};

	/// Reports a fixed external byte count without allocating during
	/// inspection.
	struct FixedReporter(
		/// Externally owned byte count returned by this test reporter.
		usize,
	);

	impl MemoryReporter for FixedReporter {
		/// Returns the externally owned byte count selected by the test.
		fn memory_allocated(&self) -> usize {
			self.0
		}
	}

	/// Checks totals, expired weak handles, explicit cleanup and registration
	/// cleanup.
	#[test]
	fn weak_reporters_do_not_pin_owners_or_leave_stale_name_entries() {
		// Tracking-mode retention tests sample this global total in parallel.
		// Keep their external contribution zero; ordinary builds exercise
		// positive aggregation through this exact same registry path.
		let (first_bytes, second_bytes) = if cfg!(feature = "allocation-tracking") {
			(0, 0)
		} else {
			(17, 31)
		};
		let first: Arc<dyn MemoryReporter> = Arc::new(FixedReporter(first_bytes));
		let second: Arc<dyn MemoryReporter> = Arc::new(FixedReporter(second_bytes));
		let first_weak = Arc::downgrade(&first);
		let second_weak = Arc::downgrade(&second);
		register_memory_reporter("registry-weak-first", first_weak.clone());
		register_memory_reporter("registry-weak-second", second_weak.clone());
		assert_eq!(Arc::strong_count(&first), 1);
		assert_eq!(Arc::strong_count(&second), 1);
		assert_eq!(memory_reporters_allocated_total(), first_bytes + second_bytes);
		let by_name = memory_reporters_allocated_by_name();
		assert_eq!(by_name.get("registry-weak-first"), Some(&first_bytes));
		assert_eq!(by_name.get("registry-weak-second"), Some(&second_bytes));
		drop(by_name);

		drop(first);
		assert!(first_weak.upgrade().is_none());
		assert_eq!(memory_reporters_allocated_total(), second_bytes);
		assert_eq!(memory_reporters_allocated_by_name().get("registry-weak-first"), Some(&0));
		cleanup_memory_reporters();
		assert!(!memory_reporters_allocated_by_name().contains_key("registry-weak-first"));

		drop(second);
		assert!(second_weak.upgrade().is_none());
		let replacement: Arc<dyn MemoryReporter> = Arc::new(FixedReporter(0));
		register_memory_reporter("registry-weak-replacement", Arc::downgrade(&replacement));
		assert_eq!(Arc::strong_count(&replacement), 1);
		assert!(!memory_reporters_allocated_by_name().contains_key("registry-weak-second"));
		drop(replacement);
		cleanup_memory_reporters();
		assert!(memory_reporters_allocated_by_name().is_empty());
		assert_eq!(memory_reporters_allocated_total(), 0);
	}
}
