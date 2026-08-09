use crate::common::hashing::HashValue;
use num::traits::ConstZero;
use num::Zero;
use std::cell::Cell;

pub trait MemoHashed {
	fn compute_hash_to_memoize(&self) -> HashValue;
}

#[derive(PartialEq)]
pub struct MemoizedHash {
	hash: Cell<HashValue>,
}

impl MemoizedHash {
	pub fn new() -> Self {
		MemoizedHash {
			hash: Cell::new(HashValue::ZERO),
		}
	}

	pub fn compute_hash(&self, value: &impl MemoHashed) -> HashValue {
		let h = self.hash.get();
		if h.is_zero() {
			let new_hash = value.compute_hash_to_memoize();
			self.hash.set(new_hash);
			new_hash
		} else {
			h
		}
	}

	pub fn hash(&self) -> HashValue {
		self.hash.get()
	}
}
