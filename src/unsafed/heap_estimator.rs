use std::mem::{align_of, size_of};

pub const OBJECT_REFERENCE_BYTES: usize = size_of::<&()>();
pub const OBJECT_ALIGNMENT_BYTES: usize = align_of::<()>();

pub fn align_object_size(size: usize) -> usize {
	(size + OBJECT_ALIGNMENT_BYTES - 1) & !(OBJECT_ALIGNMENT_BYTES - 1)
}

pub const fn shallow_size_of<T>() -> usize {
	size_of::<T>()
}

pub fn size_of_box<T: ?Sized>(b: &Box<T>) -> usize {
	size_of::<Box<T>>() + size_of_val(&**b)
}

pub fn size_of_val<T: ?Sized>(val: &T) -> usize {
	std::mem::size_of_val(val)
}

pub fn size_of_str(s: &str) -> usize {
	s.len()
}

pub fn size_of_vec<T>(vec: &Vec<T>) -> usize {
	size_of::<Vec<T>>() + vec.capacity() * size_of::<T>()
}

pub fn size_of_hashmap<K, V, S>(map: &std::collections::HashMap<K, V, S>) -> usize {
	map.capacity() * (size_of::<K>() + size_of::<V>() + size_of::<u64>()) // Approximation
}

pub fn size_of_string(s: &String) -> usize {
	shallow_size_of::<String>() + s.capacity()
}
