use std::{
	cmp::Ordering,
	hash::{Hash, Hasher},
};

/// Scalar element representation selected for a runtime numeric vector.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum VectorKind {
	/// 64-bit floating-point vector elements.
	F64,
	/// 32-bit floating-point vector elements.
	F32,
	/// 16-bit floating-point vector elements.
	F16,
	/// 64-bit signed integer vector elements.
	I64,
	/// 32-bit signed integer vector elements.
	I32,
	/// 16-bit signed integer vector elements.
	I16,
	/// 8-bit signed integer vector elements.
	I8,
	/// 8-bit unsigned integer vector elements.
	U8,
}

/// Owned numeric vector retaining its original scalar element representation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Vector {
	/// 32-bit floating-point vector elements.
	F32(
		/// 32-bit floating-point vector elements.
		Vec<f32>,
	),
	/// 64-bit floating-point vector elements.
	F64(
		/// 64-bit floating-point vector elements.
		Vec<f64>,
	),
	/// 16-bit floating-point vector elements.
	F16(
		/// 16-bit floating-point vector elements.
		Vec<half::f16>,
	),
	/// 16-bit signed integer vector elements.
	I16(
		/// 16-bit signed integer vector elements.
		Vec<i16>,
	),
	/// 32-bit signed integer vector elements.
	I32(
		/// 32-bit signed integer vector elements.
		Vec<i32>,
	),
	/// 64-bit signed integer vector elements.
	I64(
		/// 64-bit signed integer vector elements.
		Vec<i64>,
	),
	/// 8-bit signed integer vector elements.
	I8(
		/// 8-bit signed integer vector elements.
		Vec<i8>,
	),
	/// 8-bit unsigned integer vector elements.
	U8(
		/// 8-bit unsigned integer vector elements.
		Vec<u8>,
	),
}

impl Vector {
	/// Returns kind for `Vector`.
	pub const fn kind(&self) -> VectorKind {
		match self {
			Self::F32(_) => VectorKind::F32,
			Self::F64(_) => VectorKind::F64,
			Self::F16(_) => VectorKind::F16,
			Self::I16(_) => VectorKind::I16,
			Self::I32(_) => VectorKind::I32,
			Self::I64(_) => VectorKind::I64,
			Self::I8(_) => VectorKind::I8,
			Self::U8(_) => VectorKind::U8,
		}
	}

	/// Returns the logical element count of this `Vector`.
	pub const fn len(&self) -> usize {
		match self {
			Self::F32(values) => values.len(),
			Self::F64(values) => values.len(),
			Self::F16(values) => values.len(),
			Self::I16(values) => values.len(),
			Self::I32(values) => values.len(),
			Self::I64(values) => values.len(),
			Self::I8(values) => values.len(),
			Self::U8(values) => values.len(),
		}
	}

	/// Reports whether this `Vector` contains no logical elements.
	pub const fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// Borrows the element slice when this vector uses the `f32`
	/// representation.
	pub fn as_f32(&self) -> Option<&[f32]> {
		match self {
			Self::F32(values) => Some(values),
			_ => None,
		}
	}

	/// Borrows the element slice when this vector uses the `f64`
	/// representation.
	pub fn as_f64(&self) -> Option<&[f64]> {
		match self {
			Self::F64(values) => Some(values),
			_ => None,
		}
	}

	/// Borrows the element slice when this vector uses the `f16`
	/// representation.
	pub fn as_f16(&self) -> Option<&[half::f16]> {
		match self {
			Self::F16(values) => Some(values),
			_ => None,
		}
	}

	/// Borrows the element slice when this vector uses the `i16`
	/// representation.
	pub fn as_i16(&self) -> Option<&[i16]> {
		match self {
			Self::I16(values) => Some(values),
			_ => None,
		}
	}

	/// Borrows the element slice when this vector uses the `i32`
	/// representation.
	pub fn as_i32(&self) -> Option<&[i32]> {
		match self {
			Self::I32(values) => Some(values),
			_ => None,
		}
	}

	/// Borrows the element slice when this vector uses the `i64`
	/// representation.
	pub fn as_i64(&self) -> Option<&[i64]> {
		match self {
			Self::I64(values) => Some(values),
			_ => None,
		}
	}

	/// Borrows the element slice when this vector uses the `i8` representation.
	pub fn as_i8(&self) -> Option<&[i8]> {
		match self {
			Self::I8(values) => Some(values),
			_ => None,
		}
	}

	/// Borrows the element slice when this vector uses the `u8` representation.
	pub fn as_u8(&self) -> Option<&[u8]> {
		match self {
			Self::U8(values) => Some(values),
			_ => None,
		}
	}
}

impl PartialEq for Vector {
	/// Compares this `Vector` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for Vector {}

impl PartialOrd for Vector {
	/// Compares this `Vector` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Vector {
	/// Compares this `Vector` with another value using its ordering contract.
	fn cmp(&self, other: &Self) -> Ordering {
		self.kind().cmp(&other.kind()).then_with(|| match (self, other) {
			(Self::F32(left), Self::F32(right)) => compare_f32(left, right),
			(Self::F64(left), Self::F64(right)) => compare_f64(left, right),
			(Self::F16(left), Self::F16(right)) => compare_f16(left, right),
			(Self::I16(left), Self::I16(right)) => left.cmp(right),
			(Self::I32(left), Self::I32(right)) => left.cmp(right),
			(Self::I64(left), Self::I64(right)) => left.cmp(right),
			(Self::I8(left), Self::I8(right)) => left.cmp(right),
			(Self::U8(left), Self::U8(right)) => left.cmp(right),
			_ => Ordering::Equal,
		})
	}
}

impl Hash for Vector {
	/// Feeds this `Vector` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.kind().hash(state);
		match self {
			Self::F32(values) => hash_f32(values, state),
			Self::F64(values) => hash_f64(values, state),
			Self::F16(values) => hash_f16(values, state),
			Self::I16(values) => values.hash(state),
			Self::I32(values) => values.hash(state),
			Self::I64(values) => values.hash(state),
			Self::I8(values) => values.hash(state),
			Self::U8(values) => values.hash(state),
		}
	}
}

/// Returns the canonical f16 result.
fn canonical_f16(value: half::f16) -> u16 {
	if value.is_nan() {
		half::f16::NAN.to_bits()
	} else if value == half::f16::ZERO {
		half::f16::ZERO.to_bits()
	} else {
		value.to_bits()
	}
}

/// Returns the canonical f32 result.
fn canonical_f32(value: f32) -> u32 {
	if value.is_nan() {
		f32::NAN.to_bits()
	} else if value == 0.0 {
		0.0_f32.to_bits()
	} else {
		value.to_bits()
	}
}

/// Returns the canonical f64 result.
fn canonical_f64(value: f64) -> u64 {
	if value.is_nan() {
		f64::NAN.to_bits()
	} else if value == 0.0 {
		0.0_f64.to_bits()
	} else {
		value.to_bits()
	}
}

/// Compares f32.
fn compare_f32(left: &[f32], right: &[f32]) -> Ordering {
	left.iter()
		.zip(right)
		.find_map(|(left, right)| {
			let ordering = compare_f32_value(*left, *right);
			(ordering != Ordering::Equal).then_some(ordering)
		})
		.unwrap_or_else(|| left.len().cmp(&right.len()))
}

/// Compares f16.
fn compare_f16(left: &[half::f16], right: &[half::f16]) -> Ordering {
	left.iter()
		.zip(right)
		.find_map(|(left, right)| {
			let ordering = compare_f16_value(*left, *right);
			(ordering != Ordering::Equal).then_some(ordering)
		})
		.unwrap_or_else(|| left.len().cmp(&right.len()))
}

/// Compares f64.
fn compare_f64(left: &[f64], right: &[f64]) -> Ordering {
	left.iter()
		.zip(right)
		.find_map(|(left, right)| {
			let ordering = compare_f64_value(*left, *right);
			(ordering != Ordering::Equal).then_some(ordering)
		})
		.unwrap_or_else(|| left.len().cmp(&right.len()))
}

/// Compares f32 value.
fn compare_f32_value(left: f32, right: f32) -> Ordering {
	match (left.is_nan(), right.is_nan()) {
		(true, true) => Ordering::Equal,
		(true, false) => Ordering::Greater,
		(false, true) => Ordering::Less,
		(false, false) if left == 0.0 && right == 0.0 => Ordering::Equal,
		(false, false) => left.total_cmp(&right),
	}
}

/// Compares f16 value.
fn compare_f16_value(left: half::f16, right: half::f16) -> Ordering {
	match (left.is_nan(), right.is_nan()) {
		(true, true) => Ordering::Equal,
		(true, false) => Ordering::Greater,
		(false, true) => Ordering::Less,
		(false, false) if left == half::f16::ZERO && right == half::f16::ZERO => Ordering::Equal,
		(false, false) => left.total_cmp(&right),
	}
}

/// Compares f64 value.
fn compare_f64_value(left: f64, right: f64) -> Ordering {
	match (left.is_nan(), right.is_nan()) {
		(true, true) => Ordering::Equal,
		(true, false) => Ordering::Greater,
		(false, true) => Ordering::Less,
		(false, false) if left == 0.0 && right == 0.0 => Ordering::Equal,
		(false, false) => left.total_cmp(&right),
	}
}

/// Performs the hash f32 operation.
fn hash_f32<H: Hasher>(values: &[f32], state: &mut H) {
	values.len().hash(state);
	for value in values {
		canonical_f32(*value).hash(state);
	}
}

/// Performs the hash f16 operation.
fn hash_f16<H: Hasher>(values: &[half::f16], state: &mut H) {
	values.len().hash(state);
	for value in values {
		canonical_f16(*value).hash(state);
	}
}

/// Performs the hash f64 operation.
fn hash_f64<H: Hasher>(values: &[f64], state: &mut H) {
	values.len().hash(state);
	for value in values {
		canonical_f64(*value).hash(state);
	}
}

/// Expands the impl from vector declarations used by this module.
macro_rules! impl_from_vector {
	($type:ty, $variant:ident) => {
		impl From<Vec<$type>> for Vector {
			/// Converts the source payload into its corresponding value variant.
			fn from(value: Vec<$type>) -> Self {
				Self::$variant(value)
			}
		}
	};
}

impl_from_vector!(f32, F32);
impl_from_vector!(f64, F64);
impl_from_vector!(half::f16, F16);
impl_from_vector!(i16, I16);
impl_from_vector!(i32, I32);
impl_from_vector!(i64, I64);
impl_from_vector!(i8, I8);
impl_from_vector!(u8, U8);

#[cfg(test)]
/// Regression tests for vector behavior.
mod tests {
	use std::collections::HashSet;

	use super::Vector;

	/// Verifies keeps element type and dimension.
	#[test]
	fn keeps_element_type_and_dimension() {
		let vector = Vector::from(vec![1.0_f32, 2.0]);
		assert_eq!(vector.len(), 2);
		assert_eq!(vector.as_f32(), Some([1.0, 2.0].as_slice()));
		assert_eq!(vector.as_f64(), None);
	}

	/// Verifies canonicalizes float nan and zero.
	#[test]
	fn canonicalizes_float_nan_and_zero() {
		let left = Vector::from(vec![-0.0_f64, f64::NAN]);
		let right = Vector::from(vec![0.0_f64, f64::from_bits(0x7ff8_0000_0000_0001)]);
		assert_eq!(left, right);
		assert_eq!(HashSet::from([left, right]).len(), 1);
	}

	/// Verifies supports f16 and byte vectors with canonical float identity.
	#[test]
	fn supports_f16_and_byte_vectors_with_canonical_float_identity() {
		let left = Vector::from(vec![half::f16::from_f32(-0.0), half::f16::NAN]);
		let right = Vector::from(vec![half::f16::ZERO, half::f16::from_bits(0x7e01)]);
		assert_eq!(left, right);
		assert_eq!(Vector::from(vec![-1_i8, 2]).as_i8(), Some([-1, 2].as_slice()));
		assert_eq!(Vector::from(vec![1_u8, 255]).as_u8(), Some([1, 255].as_slice()));
	}
}
