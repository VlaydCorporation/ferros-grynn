use num_traits::{CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed};
use rust_decimal::Decimal;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Number,
};

/// Shared bounds for checked arithmetic helpers.
pub trait ExactArithmetic: AddExact + SubtractExact + CheckedMul {}

/// Fallible addition reporting overflow through the project status system.
pub trait AddExact: CheckedAdd {
	/// Returns add exact for `AddExact`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn add_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		add_exact(self, b)
	}
}

/// Fallible subtraction reporting overflow through the project status system.
pub trait SubtractExact: CheckedSub {
	/// Returns subtract exact for `SubtractExact`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn subtract_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		subtract_exact(self, b)
	}
}

/// Fallible multiplication reporting overflow through the project status
/// system.
pub trait MultiplyExact: CheckedMul {
	/// Returns multiply exact for `MultiplyExact`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn multiply_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		multiply_exact(self, b)
	}
}

/// Checked division helper for primitive integer operands.
pub trait DivideExact: PrimInt {
	/// Returns divide exact for `DivideExact`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn divide_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		divide_exact(self, b)
	}
}

/// Checked division helper for signed primitive integer operands.
pub trait DivideSignedExact: PrimInt + Signed {
	/// Returns divide signed exact for `DivideSignedExact`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	fn divide_signed_exact(&self, b: &Self) -> FerrosGrynnResult<Self>
	where
		Self: Sized,
	{
		divide_signed_exact(self, b)
	}
}

/// Expands the impl exact declarations used by this module.
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

/// Expands the impl div declarations used by this module.
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

/// Returns add exact for this module.
///
/// # Errors
///
/// Returns `operation_overflow` diagnostics.
pub fn add_exact<T>(a: &T, b: &T) -> FerrosGrynnResult<T>
where
	T: CheckedAdd,
{
	a.checked_add(b).ok_or(FerrosGrynnError::operation_overflow("+"))
}

/// Returns subtract exact for this module.
///
/// # Errors
///
/// Returns `operation_overflow` diagnostics.
pub fn subtract_exact<T>(a: &T, b: &T) -> FerrosGrynnResult<T>
where
	T: CheckedSub,
{
	a.checked_sub(b).ok_or(FerrosGrynnError::operation_overflow("-"))
}

/// Returns multiply exact for this module.
///
/// # Errors
///
/// Returns `operation_overflow` diagnostics.
pub fn multiply_exact<T>(a: &T, b: &T) -> FerrosGrynnResult<T>
where
	T: CheckedMul,
{
	a.checked_mul(b).ok_or(FerrosGrynnError::operation_overflow("*"))
}

/// Returns divide exact for this module.
///
/// # Errors
///
/// Returns `division_by_zero` diagnostics.
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

/// Returns divide signed exact for this module.
///
/// # Errors
///
/// Returns `division_by_zero`, `operation_overflow`, `non_exact_division`
/// diagnostics.
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

/// Returns the finite sum result.
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

/// Returns the decimal sum result.
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
