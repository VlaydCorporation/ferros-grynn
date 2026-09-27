use num_traits::{CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed};
use rust_decimal::Decimal;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Number,
};

pub trait ExactArithmetic: AddExact + SubtractExact + CheckedMul {}

pub trait AddExact: CheckedAdd {
	fn add_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		add_exact(self, b)
	}
}

pub trait SubtractExact: CheckedSub {
	fn subtract_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		subtract_exact(self, b)
	}
}

pub trait MultiplyExact: CheckedMul {
	fn multiply_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		multiply_exact(self, b)
	}
}

pub trait DivideExact: PrimInt {
	fn divide_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		divide_exact(self, b)
	}
}

pub trait DivideSignedExact: PrimInt + Signed {
	fn divide_signed_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		divide_signed_exact(self, b)
	}
}

macro_rules! impl_exact {
    ($($ident:ident),*) => {
        $(
            impl AddExact for $ident {}
            impl SubtractExact for $ident {}
            impl MultiplyExact for $ident {}
            impl ExactArithmetic for $ident {}
        )*
    };
}

macro_rules! impl_div {
    ($($ident:ident),*) => {
        $(
            impl DivideExact for $ident {}
        )*
    };
    (sig: $($ident:ident),*) => {
        $(
            impl DivideSignedExact for $ident {}
        )*
    };
}

impl_exact!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize);

impl_div!(u8, u16, u32, u64, u128, usize);
impl_div!(sig: i8, i16, i32, i64, i128, isize);

pub fn add_exact<T>(a: &T, b: &T) -> FerrosGrynnResult<T>
where
	T: CheckedAdd,
{
	a.checked_add(b).ok_or(FerrosGrynnError::operation_overflow("+"))
}

pub fn subtract_exact<T>(a: &T, b: &T) -> FerrosGrynnResult<T>
where
	T: CheckedSub,
{
	a.checked_sub(b).ok_or(FerrosGrynnError::operation_overflow("-"))
}

pub fn multiply_exact<T>(a: &T, b: &T) -> FerrosGrynnResult<T>
where
	T: CheckedMul,
{
	a.checked_mul(b).ok_or(FerrosGrynnError::operation_overflow("*"))
}

pub fn divide_exact<T>(a: &T, b: &T) -> FerrosGrynnResult<T>
where
	T: PrimInt,
{
	let (a, b) = (*a, *b);

	if b.is_zero() {
		return Err(FerrosGrynnError::division_by_zero());
	}

	if a % b != T::zero() {
		return Err(FerrosGrynnError::division_by_zero());
	}

	Ok(a / b)
}

pub fn divide_signed_exact<T>(a: &T, b: &T) -> FerrosGrynnResult<T>
where
	T: PrimInt + Signed,
{
	let (a, b) = (*a, *b);

	if b.is_zero() {
		return Err(FerrosGrynnError::division_by_zero());
	}

	// MIN / -1 overflow
	if a == T::min_value() && b == -T::one() {
		return Err(FerrosGrynnError::operation_overflow("-"));
	}

	if a % b != T::zero() {
		return Err(FerrosGrynnError::non_exact_division());
	}

	Ok(a / b)
}

/// Adds two runtime numbers with the same coercion and overflow rules used by
/// GrynnQL arithmetic.
///
/// # Errors
///
/// Returns an overflow error for an unrepresentable result and rejects the
/// intentionally unsupported decimal/float implicit conversion.
pub fn add_numbers(left: Number, right: Number) -> FerrosGrynnResult<Number> {
	match (left, right) {
		(Number::Int(left), Number::Int(right)) => left
			.checked_add(right)
			.map(Number::Int)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("+")),
		(Number::Float(left), Number::Float(right)) => finite_sum(left, right),
		(Number::Float(left), Number::Int(right)) => finite_sum(left, right as f64),
		(Number::Int(left), Number::Float(right)) => finite_sum(left as f64, right),
		(Number::Decimal(left), Number::Decimal(right)) => decimal_sum(left, right),
		(Number::Decimal(left), Number::Int(right)) => decimal_sum(left, Decimal::from(right)),
		(Number::Int(left), Number::Decimal(right)) => decimal_sum(Decimal::from(left), right),
		(Number::Float(_), Number::Decimal(_)) | (Number::Decimal(_), Number::Float(_)) => {
			Err(FerrosGrynnError::invalid_argument(
				"Float and decimal arithmetic requires an explicit cast.",
			))
		}
	}
}

fn finite_sum(left: f64, right: f64) -> FerrosGrynnResult<Number> {
	let value = left + right;
	if value.is_nan() {
		return Err(FerrosGrynnError::invalid_argument(
			"Numeric addition received non-finite operands.",
		));
	}
	if value.is_infinite() {
		return Err(FerrosGrynnError::operation_overflow("+"));
	}
	Ok(Number::Float(value))
}

fn decimal_sum(left: Decimal, right: Decimal) -> FerrosGrynnResult<Number> {
	left.checked_add(right)
		.map(Number::Decimal)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("+"))
}

/// Returns whether a runtime number is finite.
pub const fn number_is_finite(value: Number) -> bool {
	match value {
		Number::Float(value) => value.is_finite(),
		Number::Int(_) | Number::Decimal(_) => true,
	}
}
