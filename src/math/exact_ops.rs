use num_traits::{CheckedAdd, CheckedMul, CheckedSub, PrimInt, Signed};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

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

pub fn add_exact<T>(a: &T, b: &T) -> Result<T, FerrosGrynnError>
where
	T: CheckedAdd,
{
	a.checked_add(&b).ok_or(FerrosGrynnError::operation_overflow("+"))
}

pub fn subtract_exact<T>(a: &T, b: &T) -> Result<T, FerrosGrynnError>
where
	T: CheckedSub,
{
	a.checked_sub(&b).ok_or(FerrosGrynnError::operation_overflow("-"))
}

pub fn multiply_exact<T>(a: &T, b: &T) -> Result<T, FerrosGrynnError>
where
	T: CheckedMul,
{
	a.checked_mul(&b).ok_or(FerrosGrynnError::operation_overflow("*"))
}

pub fn divide_exact<T>(a: &T, b: &T) -> Result<T, FerrosGrynnError>
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

pub fn divide_signed_exact<T>(a: &T, b: &T) -> Result<T, FerrosGrynnError>
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
		return Err(FerrosGrynnError::NonExactDivision);
	}

	Ok(a / b)
}
