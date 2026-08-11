use std::{
	cmp::Ordering,
	hash::{Hash, Hasher},
};

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum VectorKind {
	F64,
	F32,
	I64,
	I32,
	I16,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Vector {
	F32(Vec<f32>),
	F64(Vec<f64>),
	I16(Vec<i16>),
	I32(Vec<i32>),
	I64(Vec<i64>),
}

impl Vector {
	pub const fn kind(&self) -> VectorKind {
		match self {
			Self::F32(_) => VectorKind::F32,
			Self::F64(_) => VectorKind::F64,
			Self::I16(_) => VectorKind::I16,
			Self::I32(_) => VectorKind::I32,
			Self::I64(_) => VectorKind::I64,
		}
	}

	pub const fn len(&self) -> usize {
		match self {
			Self::F32(values) => values.len(),
			Self::F64(values) => values.len(),
			Self::I16(values) => values.len(),
			Self::I32(values) => values.len(),
			Self::I64(values) => values.len(),
		}
	}

	pub const fn is_empty(&self) -> bool {
		self.len() == 0
	}

	pub fn as_f32(&self) -> Option<&[f32]> {
		match self {
			Self::F32(values) => Some(values),
			_ => None,
		}
	}

	pub fn as_f64(&self) -> Option<&[f64]> {
		match self {
			Self::F64(values) => Some(values),
			_ => None,
		}
	}

	pub fn as_i16(&self) -> Option<&[i16]> {
		match self {
			Self::I16(values) => Some(values),
			_ => None,
		}
	}

	pub fn as_i32(&self) -> Option<&[i32]> {
		match self {
			Self::I32(values) => Some(values),
			_ => None,
		}
	}

	pub fn as_i64(&self) -> Option<&[i64]> {
		match self {
			Self::I64(values) => Some(values),
			_ => None,
		}
	}
}

impl PartialEq for Vector {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for Vector {}

impl PartialOrd for Vector {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Vector {
	fn cmp(&self, other: &Self) -> Ordering {
		self.kind().cmp(&other.kind()).then_with(|| match (self, other) {
			(Self::F32(left), Self::F32(right)) => compare_f32(left, right),
			(Self::F64(left), Self::F64(right)) => compare_f64(left, right),
			(Self::I16(left), Self::I16(right)) => left.cmp(right),
			(Self::I32(left), Self::I32(right)) => left.cmp(right),
			(Self::I64(left), Self::I64(right)) => left.cmp(right),
			_ => Ordering::Equal,
		})
	}
}

impl Hash for Vector {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.kind().hash(state);
		match self {
			Self::F32(values) => hash_f32(values, state),
			Self::F64(values) => hash_f64(values, state),
			Self::I16(values) => values.hash(state),
			Self::I32(values) => values.hash(state),
			Self::I64(values) => values.hash(state),
		}
	}
}

fn canonical_f32(value: f32) -> u32 {
	if value.is_nan() {
		f32::NAN.to_bits()
	} else if value == 0.0 {
		0.0_f32.to_bits()
	} else {
		value.to_bits()
	}
}

fn canonical_f64(value: f64) -> u64 {
	if value.is_nan() {
		f64::NAN.to_bits()
	} else if value == 0.0 {
		0.0_f64.to_bits()
	} else {
		value.to_bits()
	}
}

fn compare_f32(left: &[f32], right: &[f32]) -> Ordering {
	left.iter()
		.zip(right)
		.find_map(|(left, right)| {
			let ordering = compare_f32_value(*left, *right);
			(ordering != Ordering::Equal).then_some(ordering)
		})
		.unwrap_or_else(|| left.len().cmp(&right.len()))
}

fn compare_f64(left: &[f64], right: &[f64]) -> Ordering {
	left.iter()
		.zip(right)
		.find_map(|(left, right)| {
			let ordering = compare_f64_value(*left, *right);
			(ordering != Ordering::Equal).then_some(ordering)
		})
		.unwrap_or_else(|| left.len().cmp(&right.len()))
}

fn compare_f32_value(left: f32, right: f32) -> Ordering {
	match (left.is_nan(), right.is_nan()) {
		(true, true) => Ordering::Equal,
		(true, false) => Ordering::Greater,
		(false, true) => Ordering::Less,
		(false, false) if left == 0.0 && right == 0.0 => Ordering::Equal,
		(false, false) => left.total_cmp(&right),
	}
}

fn compare_f64_value(left: f64, right: f64) -> Ordering {
	match (left.is_nan(), right.is_nan()) {
		(true, true) => Ordering::Equal,
		(true, false) => Ordering::Greater,
		(false, true) => Ordering::Less,
		(false, false) if left == 0.0 && right == 0.0 => Ordering::Equal,
		(false, false) => left.total_cmp(&right),
	}
}

fn hash_f32<H: Hasher>(values: &[f32], state: &mut H) {
	values.len().hash(state);
	for value in values {
		canonical_f32(*value).hash(state);
	}
}

fn hash_f64<H: Hasher>(values: &[f64], state: &mut H) {
	values.len().hash(state);
	for value in values {
		canonical_f64(*value).hash(state);
	}
}

macro_rules! impl_from_vector {
	($type:ty, $variant:ident) => {
		impl From<Vec<$type>> for Vector {
			fn from(value: Vec<$type>) -> Self {
				Self::$variant(value)
			}
		}
	};
}

impl_from_vector!(f32, F32);
impl_from_vector!(f64, F64);
impl_from_vector!(i16, I16);
impl_from_vector!(i32, I32);
impl_from_vector!(i64, I64);

#[cfg(test)]
mod tests {
	use std::collections::HashSet;

	use super::Vector;

	#[test]
	fn keeps_element_type_and_dimension() {
		let vector = Vector::from(vec![1.0_f32, 2.0]);
		assert_eq!(vector.len(), 2);
		assert_eq!(vector.as_f32(), Some([1.0, 2.0].as_slice()));
		assert_eq!(vector.as_f64(), None);
	}

	#[test]
	fn canonicalizes_float_nan_and_zero() {
		let left = Vector::from(vec![-0.0_f64, f64::NAN]);
		let right = Vector::from(vec![0.0_f64, f64::from_bits(0x7ff8_0000_0000_0001)]);
		assert_eq!(left, right);
		assert_eq!(HashSet::from([left, right]).len(), 1);
	}
}
