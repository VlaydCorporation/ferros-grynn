use num::Zero;
use num::traits::{ConstOne, ConstZero};
use std::cell::Cell;
use std::hash::Hash;

pub const HASH_CONSTANT: HashValue = HashValue::HASH_CONSTANT;

mod hash_value {
	use num::traits::{ConstOne, ConstZero};
	use num::{One, Zero};
	use std::cmp::Ordering;
	use std::fmt::{Display, Formatter};
	use std::ops::{Add, AddAssign, Mul, MulAssign};

	#[derive(Debug, PartialEq, Eq, Ord, PartialOrd, Hash, Clone, Copy)]
	pub struct HashValue(u32);

	impl HashValue {
		pub const HASH_CONSTANT: Self = HashValue(31);
	}

	impl PartialEq<u32> for HashValue {
		fn eq(&self, other: &u32) -> bool {
			self.0.eq(other)
		}
	}

	impl PartialOrd<u32> for HashValue {
		fn partial_cmp(&self, other: &u32) -> Option<Ordering> {
			self.0.partial_cmp(other)
		}
	}

	impl From<u32> for HashValue {
		fn from(value: u32) -> Self {
			Self(value)
		}
	}

	impl Add for HashValue {
		type Output = HashValue;

		fn add(self, rhs: Self) -> Self::Output {
			HashValue(self.0.wrapping_add(rhs.0))
		}
	}

	impl Add<u32> for HashValue {
		type Output = HashValue;

		fn add(self, rhs: u32) -> Self::Output {
			self.add(HashValue(rhs))
		}
	}

	impl Mul for HashValue {
		type Output = HashValue;

		fn mul(self, rhs: Self) -> Self::Output {
			HashValue(self.0.wrapping_mul(rhs.0))
		}
	}

	impl Mul<u32> for HashValue {
		type Output = HashValue;

		fn mul(self, rhs: u32) -> Self::Output {
			self.mul(HashValue(rhs))
		}
	}

	impl AddAssign for HashValue {
		fn add_assign(&mut self, rhs: Self) {
			self.0 += rhs.0;
		}
	}

	impl MulAssign for HashValue {
		fn mul_assign(&mut self, rhs: Self) {
			self.0 *= rhs.0;
		}
	}

	impl ConstZero for HashValue {
		const ZERO: Self = HashValue(0);
	}

	impl ConstOne for HashValue {
		const ONE: Self = HashValue(1);
	}

	impl Zero for HashValue {
		fn zero() -> Self {
			HashValue::ZERO
		}

		fn is_zero(&self) -> bool {
			self.0 == 0
		}
	}

	impl One for HashValue {
		fn one() -> Self {
			HashValue::ONE
		}
	}

	// impl Default for HashValue {
	//     fn default() -> Self {
	//         Self::ZERO
	//     }
	// }

	impl Display for HashValue {
		fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
			write!(f, "{}", self.0)
		}
	}

	impl From<HashValue> for u64 {
		fn from(value: HashValue) -> Self {
			value.0 as u64
		}
	}
}

use crate::values::AnyValue;
pub use hash_value::*;

pub trait Hashable {
	fn hash(&self) -> HashValue;
}

#[macro_export]
macro_rules! impl_simple_hashable {
    ($($ident:ident),+) => {
		$(
			impl Hashable for $ident {
				fn hash(&self) -> HashValue {
					self.hash.compute_hash(self)
				}
			}
		)*
	};
}

#[macro_export]
macro_rules! impl_id_hash {
    ($($ident:ident),+) => {
		$(
			impl MemoHashed for $ident {
				fn compute_hash_to_memoize(&self) -> HashValue {
					hash_u64(self.id())
				}
			}
		)*
	};
}

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

pub fn hash_bool(v: bool) -> HashValue {
	if v {
		HashValue::from(1231)
	} else {
		HashValue::from(1237)
	}
}

pub fn hash_u64(v: u64) -> HashValue {
	(((v ^ (v >> 32)) & 0xFFFF_FFFF) as u32).into()
}

pub fn hash_str(v: &str) -> HashValue {
	let mut h = HashValue::ZERO;
	v.encode_utf16().for_each(|c| {
		h = HASH_CONSTANT * h + (c as u32);
	});
	h
}

pub fn hash_hashables<H:Hashable, I: Iterator<Item = H>>(values: I) -> HashValue {
	let mut h = HashValue::ONE;
	values.for_each(|v| h = HASH_CONSTANT * h + v.hash());
	h
}
