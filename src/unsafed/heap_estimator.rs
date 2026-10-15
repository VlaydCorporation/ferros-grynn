use std::mem::{align_of, size_of};

/// Native reference width in bytes used by the disconnected heap-size
/// estimator.
pub const OBJECT_REFERENCE_BYTES: usize = size_of::<&()>();
/// Unit-type alignment used by the estimator's rounding formula; currently one
/// byte.
pub const OBJECT_ALIGNMENT_BYTES: usize = align_of::<()>();

/// Returns align object size for this module.
pub fn align_object_size(size: usize) -> usize {
	(size + OBJECT_ALIGNMENT_BYTES - 1) & !(OBJECT_ALIGNMENT_BYTES - 1)
}

/// Returns shallow size of for this module.
pub const fn shallow_size_of<T>() -> usize {
	size_of::<T>()
}

/// Returns size of box for this module.
pub fn size_of_box<T: ?Sized>(b: &Box<T>) -> usize {
	size_of::<Box<T>>() + size_of_val(&**b)
}

/// Returns size of val for this module.
pub fn size_of_val<T: ?Sized>(val: &T) -> usize {
	std::mem::size_of_val(val)
}

/// Returns size of str for this module.
pub fn size_of_str(s: &str) -> usize {
	s.len()
}

/// Returns size of vec for this module.
pub fn size_of_vec<T>(vec: &Vec<T>) -> usize {
	size_of::<Vec<T>>() + vec.capacity() * size_of::<T>()
}

/// Estimates capacity-based HashMap storage; bucket metadata and allocator
/// overhead are approximate.
pub fn size_of_hashmap<K, V, S>(map: &std::collections::HashMap<K, V, S>) -> usize {
	map.capacity() * (size_of::<K>() + size_of::<V>() + size_of::<u64>()) // Approximation
}

/// Returns size of string for this module.
pub fn size_of_string(s: &String) -> usize {
	shallow_size_of::<String>() + s.capacity()
}
