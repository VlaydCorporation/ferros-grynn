use std::{
	cmp::Ordering,
	fmt::{self, Display, Formatter},
	hash::{Hash, Hasher},
	str::FromStr,
};

use num::{BigInt, Integer};
use num_traits::{FromPrimitive, ToPrimitive, Zero};
use rust_decimal::Decimal;

use crate::gql_status::FerrosGrynnError;

/// Integer, floating-point or decimal value with shared numeric comparison
/// semantics.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Number {
	/// Runtime representation of a signed integer.
	Int(
		/// Runtime representation of a signed integer.
		i64,
	),
	/// Runtime representation of a floating-point number.
	Float(
		/// Runtime representation of a floating-point number.
		f64,
	),
	/// Runtime representation of a decimal number.
	Decimal(
		/// Runtime representation of a decimal number.
		Decimal,
	),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Canonical number alternatives.
enum CanonicalNumber {
	/// Negative infinity case of canonical number.
	NegativeInfinity,
	/// Finite case of canonical number.
	Finite {
		/// Numerator associated with this canonical number.
		numerator: BigInt,
		/// Denominator associated with this canonical number.
		denominator: BigInt,
	},
	/// Positive infinity case of canonical number.
	PositiveInfinity,
	/// Na n case of canonical number.
	NaN,
}

impl PartialOrd for CanonicalNumber {
	/// Compares this `CanonicalNumber` with another value when they are
	/// ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for CanonicalNumber {
	/// Compares this `CanonicalNumber` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		match (self, other) {
			(Self::NegativeInfinity, Self::NegativeInfinity)
			| (Self::PositiveInfinity, Self::PositiveInfinity)
			| (Self::NaN, Self::NaN) => Ordering::Equal,
			(Self::NegativeInfinity, _) | (_, Self::NaN) => Ordering::Less,
			(_, Self::NegativeInfinity) | (Self::NaN, _) => Ordering::Greater,
			(Self::PositiveInfinity, _) => Ordering::Greater,
			(_, Self::PositiveInfinity) => Ordering::Less,
			(
				Self::Finite {
					numerator: left_numerator,
					denominator: left_denominator,
				},
				Self::Finite {
					numerator: right_numerator,
					denominator: right_denominator,
				},
			) => (left_numerator * right_denominator).cmp(&(right_numerator * left_denominator)),
		}
	}
}

impl Number {
	/// Canonical floating-point not-a-number runtime value.
	pub const NAN: Self = Self::Float(f64::NAN);

	/// Reports whether the value is nan for `Number`.
	pub const fn is_nan(self) -> bool {
		matches!(self, Self::Float(value) if value.is_nan())
	}

	/// Reports whether the value is int for `Number`.
	pub const fn is_int(self) -> bool {
		matches!(self, Self::Int(_))
	}

	/// Reports whether the value is float for `Number`.
	pub const fn is_float(self) -> bool {
		matches!(self, Self::Float(_))
	}

	/// Reports whether the value is decimal for `Number`.
	pub const fn is_decimal(self) -> bool {
		matches!(self, Self::Decimal(_))
	}

	/// Reports whether the value is zero for `Number`.
	pub fn is_zero(self) -> bool {
		match self {
			Self::Int(value) => value == 0,
			Self::Float(value) => value == 0.0,
			Self::Decimal(value) => value.is_zero(),
		}
	}

	/// Attempts an exact conversion to `i64`, returning `None` when it cannot
	/// be represented.
	pub fn as_i64(self) -> Option<i64> {
		i64::try_from(self).ok()
	}

	/// Attempts an exact conversion to `f64`, returning `None` when it cannot
	/// be represented.
	pub fn as_f64(self) -> Option<f64> {
		f64::try_from(self).ok()
	}

	/// Attempts an exact conversion to `Decimal`, returning `None` when it
	/// cannot be represented.
	pub fn as_decimal(self) -> Option<Decimal> {
		Decimal::try_from(self).ok()
	}

	/// Returns the canonical result.
	fn canonical(self) -> CanonicalNumber {
		match self {
			Self::Int(value) => CanonicalNumber::Finite {
				numerator: BigInt::from(value),
				denominator: BigInt::from(1),
			},
			Self::Float(value) => canonical_float(value),
			Self::Decimal(value) => {
				let numerator = BigInt::from(value.mantissa());
				let denominator = BigInt::from(10_u8).pow(value.scale());
				canonical_fraction(numerator, denominator)
			}
		}
	}

	/// Returns the exact integer result.
	fn exact_integer(self) -> Option<BigInt> {
		match self.canonical() {
			CanonicalNumber::Finite {
				numerator,
				denominator,
			} if denominator == BigInt::from(1) => Some(numerator),
			_ => None,
		}
	}
}

/// Returns the canonical fraction result.
fn canonical_fraction(numerator: BigInt, denominator: BigInt) -> CanonicalNumber {
	if numerator.is_zero() {
		return CanonicalNumber::Finite {
			numerator,
			denominator: BigInt::from(1),
		};
	}

	let divisor = numerator.gcd(&denominator);
	CanonicalNumber::Finite {
		numerator: numerator / &divisor,
		denominator: denominator / divisor,
	}
}

/// Returns the canonical float result.
fn canonical_float(value: f64) -> CanonicalNumber {
	if value.is_nan() {
		return CanonicalNumber::NaN;
	}
	if value == f64::NEG_INFINITY {
		return CanonicalNumber::NegativeInfinity;
	}
	if value == f64::INFINITY {
		return CanonicalNumber::PositiveInfinity;
	}
	if value == 0.0 {
		return canonical_fraction(BigInt::from(0), BigInt::from(1));
	}

	let bits = value.to_bits();
	let negative = bits >> 63 != 0;
	let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
	let fraction = bits & ((1_u64 << 52) - 1);
	let (mantissa, exponent) = if raw_exponent == 0 {
		(fraction, -1074)
	} else {
		((1_u64 << 52) | fraction, raw_exponent - 1023 - 52)
	};

	let mut numerator = BigInt::from(mantissa);
	let denominator = if exponent >= 0 {
		numerator <<= exponent as usize;
		BigInt::from(1)
	} else {
		BigInt::from(1) << (-exponent) as usize
	};
	if negative {
		numerator = -numerator;
	}

	canonical_fraction(numerator, denominator)
}

/// Returns the conversion error result.
fn conversion_error(value: Number, target: &'static str) -> FerrosGrynnError {
	FerrosGrynnError::conversion_error(value.to_string(), target)
}

impl Default for Number {
	/// Constructs the default initial state of `Number`.
	fn default() -> Self {
		Self::Int(0)
	}
}

impl Display for Number {
	/// Formats `Number` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::Int(value) => Display::fmt(value, f),
			Self::Float(value) => Display::fmt(value, f),
			Self::Decimal(value) => Display::fmt(value, f),
		}
	}
}

impl PartialEq for Number {
	/// Compares this `Number` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.canonical() == other.canonical()
	}
}

impl Eq for Number {}

impl PartialOrd for Number {
	/// Compares this `Number` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Number {
	/// Compares this `Number` with another value using its ordering contract.
	fn cmp(&self, other: &Self) -> Ordering {
		match (self, other) {
			(Self::Int(left), Self::Int(right)) => left.cmp(right),
			_ => self.canonical().cmp(&other.canonical()),
		}
	}
}

impl Hash for Number {
	/// Feeds this `Number` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.canonical().hash(state);
	}
}

/// Expands the from primitive int declarations used by this module.
macro_rules! from_primitive_int {
	($($type:ty),* $(,)?) => {
		$(
			impl From<$type> for Number {
				/// Converts the source payload into its corresponding value variant.
				fn from(value: $type) -> Self {
					Self::Int(i64::from(value))
				}
			}
		)*
	};
}

from_primitive_int!(i8, i16, i32, i64, u8, u16, u32);

impl From<isize> for Number {
	/// Converts `isize` into `Number`.
	fn from(value: isize) -> Self {
		Self::Int(value as i64)
	}
}

impl From<u64> for Number {
	/// Converts `u64` into `Number`.
	fn from(value: u64) -> Self {
		i64::try_from(value).map_or_else(|_| Self::Decimal(Decimal::from(value)), Self::Int)
	}
}

impl From<usize> for Number {
	/// Converts `usize` into `Number`.
	fn from(value: usize) -> Self {
		Self::from(value as u64)
	}
}

impl TryFrom<i128> for Number {
	type Error = FerrosGrynnError;

	/// Converts `i128` into `Number` after validating the conversion.
	fn try_from(value: i128) -> Result<Self, Self::Error> {
		if let Ok(value) = i64::try_from(value) {
			Ok(Self::Int(value))
		} else if let Some(value) = Decimal::from_i128(value) {
			Ok(Self::Decimal(value))
		} else {
			Err(FerrosGrynnError::conversion_error(value.to_string(), "Number"))
		}
	}
}

impl TryFrom<u128> for Number {
	type Error = FerrosGrynnError;

	/// Converts `u128` into `Number` after validating the conversion.
	fn try_from(value: u128) -> Result<Self, Self::Error> {
		if let Ok(value) = i64::try_from(value) {
			Ok(Self::Int(value))
		} else if let Some(value) = Decimal::from_u128(value) {
			Ok(Self::Decimal(value))
		} else {
			Err(FerrosGrynnError::conversion_error(value.to_string(), "Number"))
		}
	}
}

impl From<f32> for Number {
	/// Converts `f32` into `Number`.
	fn from(value: f32) -> Self {
		Self::Float(f64::from(value))
	}
}

impl From<f64> for Number {
	/// Converts `f64` into `Number`.
	fn from(value: f64) -> Self {
		Self::Float(value)
	}
}

impl From<Decimal> for Number {
	/// Converts `Decimal` into `Number`.
	fn from(value: Decimal) -> Self {
		Self::Decimal(value)
	}
}

impl FromStr for Number {
	type Err = FerrosGrynnError;

	/// Constructs from str for `Number`.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		if let Some(value) = value.strip_suffix("dec") {
			return value
				.parse::<Decimal>()
				.map(Self::Decimal)
				.map_err(|_err| FerrosGrynnError::conversion_error(value, "Number"));
		}
		if let Ok(value) = value.parse::<i64>() {
			return Ok(Self::Int(value));
		}
		if !value.contains('.')
			&& !value.contains('e')
			&& !value.contains('E')
			&& let Ok(value) = value.parse::<Decimal>()
		{
			return Ok(Self::Decimal(value));
		}
		if let Ok(value) = value.parse::<f64>() {
			return Ok(Self::Float(value));
		}

		value
			.parse::<Decimal>()
			.map(Self::Decimal)
			.map_err(|_err| FerrosGrynnError::conversion_error(value, "Number"))
	}
}

/// Expands the try into integer declarations used by this module.
macro_rules! try_into_integer {
	($($type:ty => $method:ident),* $(,)?) => {
		$(
			impl TryFrom<Number> for $type {
				type Error = FerrosGrynnError;

				/// Checks the source variant and converts its payload to the requested type.
				fn try_from(value: Number) -> Result<Self, Self::Error> {
					value
						.exact_integer()
						.and_then(|value| value.$method())
						.ok_or_else(|| conversion_error(value, stringify!($type)))
				}
			}
		)*
	};
}

try_into_integer!(
	i8 => to_i8,
	i16 => to_i16,
	i32 => to_i32,
	i64 => to_i64,
	i128 => to_i128,
	isize => to_isize,
	u8 => to_u8,
	u16 => to_u16,
	u32 => to_u32,
	u64 => to_u64,
	u128 => to_u128,
	usize => to_usize,
);

impl TryFrom<Number> for f64 {
	type Error = FerrosGrynnError;

	/// Converts `Number` into `f64` after validating the conversion.
	fn try_from(value: Number) -> Result<Self, Self::Error> {
		if let Number::Float(value) = value {
			return Ok(value);
		}

		let candidate = match value {
			Number::Int(value) => value.to_f64(),
			Number::Decimal(value) => value.to_f64(),
			Number::Float(_) => unreachable!(),
		}
		.ok_or_else(|| conversion_error(value, "f64"))?;

		(Number::Float(candidate) == value)
			.then_some(candidate)
			.ok_or_else(|| conversion_error(value, "f64"))
	}
}

impl TryFrom<Number> for f32 {
	type Error = FerrosGrynnError;

	/// Converts `Number` into `f32` after validating the conversion.
	fn try_from(value: Number) -> Result<Self, Self::Error> {
		let candidate = match value {
			Number::Int(value) => value.to_f32(),
			Number::Float(value) => value.to_f32(),
			Number::Decimal(value) => value.to_f32(),
		}
		.ok_or_else(|| conversion_error(value, "f32"))?;

		(Number::Float(f64::from(candidate)) == value)
			.then_some(candidate)
			.ok_or_else(|| conversion_error(value, "f32"))
	}
}

impl TryFrom<Number> for Decimal {
	type Error = FerrosGrynnError;

	/// Converts `Number` into `Decimal` after validating the conversion.
	fn try_from(value: Number) -> Result<Self, Self::Error> {
		match value {
			Number::Int(value) => Ok(Self::from(value)),
			Number::Decimal(value) => Ok(value),
			Number::Float(float) => {
				let decimal =
					Self::from_f64(float).ok_or_else(|| conversion_error(value, "Decimal"))?;
				(Number::Decimal(decimal) == value)
					.then_some(decimal)
					.ok_or_else(|| conversion_error(value, "Decimal"))
			}
		}
	}
}

#[cfg(test)]
/// Regression tests for number behavior.
mod tests {
	use std::collections::{BTreeSet, HashSet};

	use rust_decimal::Decimal;

	use super::Number;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies compares cross variant values exactly.
	#[test]
	fn compares_cross_variant_values_exactly() {
		assert_eq!(Number::Int(1), Number::Float(1.0));
		assert_eq!(Number::Int(1), Number::Decimal(Decimal::ONE));
		assert_ne!(Number::Float(0.1), Number::Decimal(Decimal::new(1, 1)));
		assert!(Number::Decimal(Decimal::new(2, 2)) > Number::Decimal(Decimal::new(3, 3)));
		assert!(Number::Decimal(Decimal::new(-2, 2)) < Number::Decimal(Decimal::new(-3, 3)));
		assert!(Number::Float(f64::INFINITY) < Number::NAN);
	}

	/// Verifies canonicalizes nan and signed zero for eq ord and hash.
	#[test]
	fn canonicalizes_nan_and_signed_zero_for_eq_ord_and_hash() {
		let nan_with_payload = Number::Float(f64::from_bits(0x7ff8_0000_0000_0001));
		assert_eq!(Number::NAN, nan_with_payload);
		assert_eq!(Number::Float(-0.0), Number::Int(0));

		let hash_values = HashSet::from([
			Number::NAN,
			nan_with_payload,
			Number::Float(-0.0),
			Number::Decimal(Decimal::ZERO),
		]);
		assert_eq!(hash_values.len(), 2);

		let ordered_values =
			BTreeSet::from([Number::Int(0), Number::Float(-0.0), Number::Decimal(Decimal::ZERO)]);
		assert_eq!(ordered_values.len(), 1);
	}

	/// Verifies rejects lossy conversions.
	#[test]
	fn rejects_lossy_conversions() {
		assert!(i64::try_from(Number::Float(1.5)).is_err());
		assert!(f64::try_from(Number::Int(9_007_199_254_740_993)).is_err());
		assert_eq!(i64::try_from(Number::Float(1.0)).assert_ok(), 1);
	}

	/// Verifies parses explicit decimal suffix.
	#[test]
	fn parses_explicit_decimal_suffix() {
		assert_eq!("0.1dec".parse::<Number>().assert_ok(), Number::Decimal(Decimal::new(1, 1)),);
		assert!(matches!("0.1".parse::<Number>().assert_ok(), Number::Float(_)));
		assert!(matches!("9223372036854775808".parse::<Number>().assert_ok(), Number::Decimal(_),));
	}
}
