#![allow(
	clippy::len_without_is_empty,
	clippy::iter_without_into_iter,
	reason = "The existing workspace-only collection contract stays unchanged during extraction"
)]
#![allow(
	clippy::rc_buffer,
	reason = "Vec is required for chunked copy-on-write growth and truncation"
)]

use std::{
	iter::FusedIterator,
	ops::{Index, IndexMut},
	sync::Arc,
};

/// Default number of elements per copy-on-write chunk.
const DEFAULT_CHUNK_LEN: usize = 256;

/// A vector whose clones share immutable chunks until one of them is changed.
#[derive(Clone, Debug)]
pub struct CowVec<T, const CHUNK_LEN: usize = DEFAULT_CHUNK_LEN> {
	/// Number of logical elements currently stored.
	len: usize,
	/// Chunks containing the logical elements.
	chunks: Arc<Vec<Arc<Vec<T>>>>,
}

impl<T, const CHUNK_LEN: usize> CowVec<T, CHUNK_LEN> {
	/// Creates a new `CowVec<T, CHUNK_LEN>` value.
	///
	/// # Panics
	/// Panics if the configured chunk length is zero.
	pub fn new() -> Self {
		assert!(CHUNK_LEN > 0, "CowVec chunks must not be empty");
		Self {
			len: 0,
			chunks: Arc::new(Vec::new()),
		}
	}

	/// Configures capacity.
	///
	/// # Panics
	/// Panics if the configured chunk length is zero.
	pub fn with_capacity(capacity: usize) -> Self {
		assert!(CHUNK_LEN > 0, "CowVec chunks must not be empty");
		Self {
			len: 0,
			chunks: Arc::new(Vec::with_capacity(capacity.div_ceil(CHUNK_LEN))),
		}
	}

	/// Returns the number of logical elements.
	pub const fn len(&self) -> usize {
		self.len
	}

	/// Returns cow vec<t, chunk len>.
	pub fn get(&self, index: usize) -> Option<&T> {
		(index < self.len).then(|| &self.chunks[index / CHUNK_LEN][index % CHUNK_LEN])
	}

	/// Returns an iterator over the stored elements.
	pub fn iter(&self) -> Iter<'_, T> {
		Iter {
			chunks: self.chunks.iter(),
			current: None,
			remaining: self.len,
		}
	}

	/// Converts to vec.
	pub fn to_vec(&self) -> Vec<T>
	where
		T: Clone,
	{
		self.iter().cloned().collect()
	}
}

impl<T: Clone, const CHUNK_LEN: usize> CowVec<T, CHUNK_LEN> {
	/// Returns mut.
	pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
		if index >= self.len {
			return None;
		}
		// Clone the shared chunk directory first; only the written chunk then
		// copies its elements.
		let chunks = Arc::make_mut(&mut self.chunks);
		Some(&mut Arc::make_mut(&mut chunks[index / CHUNK_LEN])[index % CHUNK_LEN])
	}

	/// Pushes cow vec<t, chunk len>.
	pub fn push(&mut self, value: T) {
		let chunks = Arc::make_mut(&mut self.chunks);
		let offset = self.len % CHUNK_LEN;
		if offset == 0 {
			chunks.push(Arc::new(Vec::with_capacity(CHUNK_LEN)));
		}
		let chunk_index = self.len / CHUNK_LEN;
		Arc::make_mut(&mut chunks[chunk_index]).push(value);
		self.len += 1;
	}

	/// Pops cow vec<t, chunk len>.
	pub fn pop(&mut self) -> Option<T> {
		if self.len == 0 {
			return None;
		}
		let chunks = Arc::make_mut(&mut self.chunks);
		let chunk = Arc::make_mut(chunks.last_mut()?);
		let value = chunk.pop()?;
		self.len -= 1;
		if chunk.is_empty() {
			chunks.pop();
		}
		Some(value)
	}

	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "the bounds check guarantees a non-empty CowVec"
	)]
	/// Swaps remove.
	pub fn swap_remove(&mut self, index: usize) -> T {
		assert!(index < self.len, "swap_remove index out of bounds");

		let last = self.pop().expect("the bounds check guarantees a non-empty CowVec");
		if index == self.len {
			return last;
		}
		std::mem::replace(&mut self[index], last)
	}

	/// Resizes with.
	#[allow(
		clippy::impl_trait_in_params,
		reason = "Exact-version internal call signatures remain unchanged during extraction"
	)]
	pub fn resize_with(&mut self, new_len: usize, mut value: impl FnMut() -> T) {
		while self.len < new_len {
			self.push(value());
		}
		while self.len > new_len {
			self.pop();
		}
	}

	/// Resizes cow vec<t, chunk len>.
	pub fn resize(&mut self, new_len: usize, value: T) {
		self.resize_with(new_len, || value.clone());
	}

	/// Performs the fill operation.
	pub fn fill(&mut self, value: T) {
		for chunk in Arc::make_mut(&mut self.chunks) {
			Arc::make_mut(chunk).fill(value.clone());
		}
	}

	/// Clears cow vec<t, chunk len>.
	pub fn clear(&mut self) {
		self.len = 0;
		self.chunks = Arc::new(Vec::new());
	}

	/// Retains cow vec<t, chunk len>.
	#[allow(
		clippy::impl_trait_in_params,
		reason = "Exact-version internal call signatures remain unchanged during extraction"
	)]
	pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
		let mut retained = Self::with_capacity(self.len);
		for value in self.iter().filter(|value| keep(value)) {
			retained.push(value.clone());
		}
		*self = retained;
	}
}

impl<T: Clone, const CHUNK_LEN: usize> From<Vec<T>> for CowVec<T, CHUNK_LEN> {
	/// Converts `Vec<T>` into `CowVec<T, CHUNK_LEN>`.
	fn from(values: Vec<T>) -> Self {
		let mut result = Self::with_capacity(values.len());
		for value in values {
			result.push(value);
		}
		result
	}
}

impl<T, const CHUNK_LEN: usize> Default for CowVec<T, CHUNK_LEN> {
	/// Constructs the default initial state of `CowVec<T, CHUNK_LEN>`.
	fn default() -> Self {
		Self::new()
	}
}

impl<T, const CHUNK_LEN: usize> Index<usize> for CowVec<T, CHUNK_LEN> {
	type Output = T;

	/// Returns index for `CowVec<T, CHUNK_LEN>`.
	fn index(&self, index: usize) -> &Self::Output {
		&self.chunks[index / CHUNK_LEN][index % CHUNK_LEN]
	}
}

impl<T: Clone, const CHUNK_LEN: usize> IndexMut<usize> for CowVec<T, CHUNK_LEN> {
	/// Returns index mut for `CowVec<T, CHUNK_LEN>`.
	fn index_mut(&mut self, index: usize) -> &mut Self::Output {
		let chunks = Arc::make_mut(&mut self.chunks);
		&mut Arc::make_mut(&mut chunks[index / CHUNK_LEN])[index % CHUNK_LEN]
	}
}

/// Iter state.
pub struct Iter<'a, T> {
	/// Chunks containing the logical elements.
	chunks: std::slice::Iter<'a, Arc<Vec<T>>>,
	/// Current associated with this iter.
	current: Option<std::slice::Iter<'a, T>>,
	/// Number of elements that have not yet been consumed.
	remaining: usize,
}

impl<'a, T> Iterator for Iter<'a, T> {
	type Item = &'a T;

	/// Advances this `Iter<'a, T>` and returns its next element, if any.
	fn next(&mut self) -> Option<Self::Item> {
		loop {
			if let Some(value) = self.current.as_mut().and_then(Iterator::next) {
				self.remaining -= 1;
				return Some(value);
			}
			self.current = Some(self.chunks.next()?.iter());
		}
	}

	/// Returns lower and optional upper bounds on the remaining element count.
	fn size_hint(&self) -> (usize, Option<usize>) {
		(self.remaining, Some(self.remaining))
	}
}

impl<T> ExactSizeIterator for Iter<'_, T> {}
impl<T> FusedIterator for Iter<'_, T> {}

#[cfg(test)]
/// Regression tests for cow vec behavior.
mod tests {
	use std::sync::Arc;

	use super::CowVec;

	/// Verifies clone copies only the changed chunk.
	#[test]
	fn clone_copies_only_the_changed_chunk() {
		let mut original = CowVec::<usize, 2>::new();
		for value in 0..5 {
			original.push(value);
		}

		let mut changed = original.clone();
		changed[2] = 9;

		assert_eq!(original.iter().copied().collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
		assert_eq!(changed.iter().copied().collect::<Vec<_>>(), [0, 1, 9, 3, 4]);
		assert!(Arc::ptr_eq(&original.chunks[0], &changed.chunks[0]));
		assert!(!Arc::ptr_eq(&original.chunks[1], &changed.chunks[1]));
		assert!(Arc::ptr_eq(&original.chunks[2], &changed.chunks[2]));
	}
}
