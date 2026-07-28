use std::{
	fmt::{self, Display, Formatter},
	marker::PhantomData,
};

use serde::{Deserialize, Serialize};

/// Formats the status parameter without intermediate `String` allocation.
pub trait ParameterFormatter<T: ?Sized> {
	fn fmt(value: &T, f: &mut Formatter<'_>) -> fmt::Result;
}

/// Adapter of the parameter to the standard [`Display`].
pub struct Formatted<'a, F, T: ?Sized> {
	value: &'a T,
	formatter: PhantomData<F>,
}

impl<'a, F, T: ?Sized> Formatted<'a, F, T> {
	pub const fn new(value: &'a T) -> Self {
		Self {
			value,
			formatter: PhantomData,
		}
	}
}

impl<F, T: ?Sized> Display for Formatted<'_, F, T>
where
	F: ParameterFormatter<T>,
{
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		F::fmt(self.value, f)
	}
}

pub struct DisplayValue;
pub struct Identifier;
pub struct StringLiteral;
pub struct Callable;
pub struct QueryParameter;
pub struct ValueType;

impl<T: Display + ?Sized> ParameterFormatter<T> for DisplayValue {
	fn fmt(value: &T, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(value, f)
	}
}

fn write_escaped(value: &str, f: &mut Formatter<'_>, delimiter: char) -> fmt::Result {
	write!(f, "{delimiter}")?;
	for character in value.chars() {
		if character == delimiter {
			write!(f, "{delimiter}{delimiter}")?;
		} else {
			write!(f, "{character}")?;
		}
	}
	write!(f, "{delimiter}")
}

impl<T: Display + ?Sized> ParameterFormatter<T> for Identifier {
	fn fmt(value: &T, f: &mut Formatter<'_>) -> fmt::Result {
		write_escaped(&value.to_string(), f, '`')
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for StringLiteral {
	fn fmt(value: &T, f: &mut Formatter<'_>) -> fmt::Result {
		write_escaped(&value.to_string(), f, '\'')
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for Callable {
	fn fmt(value: &T, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{value}()")
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for QueryParameter {
	fn fmt(value: &T, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str("$")?;
		Identifier::fmt(value, f)
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for ValueType {
	fn fmt(value: &T, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(value, f)
	}
}

pub trait JoinSeparator {
	fn write_between(f: &mut Formatter<'_>) -> fmt::Result;
	fn write_last(f: &mut Formatter<'_>, has_multiple_prefix_items: bool) -> fmt::Result;
}

pub struct Comma;
pub struct And;
pub struct Or;

impl JoinSeparator for Comma {
	fn write_between(f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(", ")
	}

	fn write_last(f: &mut Formatter<'_>, _has_multiple_prefix_items: bool) -> fmt::Result {
		f.write_str(", ")
	}
}

impl JoinSeparator for And {
	fn write_between(f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(", ")
	}

	fn write_last(f: &mut Formatter<'_>, has_multiple_prefix_items: bool) -> fmt::Result {
		if has_multiple_prefix_items {
			f.write_str(", and ")
		} else {
			f.write_str(" and ")
		}
	}
}

impl JoinSeparator for Or {
	fn write_between(f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(", ")
	}

	fn write_last(f: &mut Formatter<'_>, has_multiple_prefix_items: bool) -> fmt::Result {
		if has_multiple_prefix_items {
			f.write_str(", or ")
		} else {
			f.write_str(" or ")
		}
	}
}

pub struct Join<S, F>(PhantomData<(S, F)>);

impl<S, F, T> ParameterFormatter<[T]> for Join<S, F>
where
	S: JoinSeparator,
	F: ParameterFormatter<T>,
{
	fn fmt(values: &[T], f: &mut Formatter<'_>) -> fmt::Result {
		for (index, value) in values.iter().enumerate() {
			if index > 0 {
				if index + 1 == values.len() {
					S::write_last(f, values.len() > 2)?;
				} else {
					S::write_between(f)?;
				}
			}
			F::fmt(value, f)?;
		}
		Ok(())
	}
}

impl<S, F, T> ParameterFormatter<Vec<T>> for Join<S, F>
where
	S: JoinSeparator,
	F: ParameterFormatter<T>,
{
	fn fmt(values: &Vec<T>, f: &mut Formatter<'_>) -> fmt::Result {
		<Self as ParameterFormatter<[T]>>::fmt(values, f)
	}
}

/// The format is an independent parameter value for wire DTO diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticValue {
	Null,
	Bool(bool),
	Integer(i64),
	Unsigned(u64),
	Float(f64),
	String(String),
	List(Vec<DiagnosticValue>),
	Map(std::collections::BTreeMap<String, DiagnosticValue>),
}

/// Converts a typed status parameter to a value for inspection.
pub trait StatusParameter {
	fn to_diagnostic_value(&self) -> DiagnosticValue;
}

impl StatusParameter for String {
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::String(self.clone())
	}
}

impl StatusParameter for str {
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::String(self.to_owned())
	}
}

impl StatusParameter for bool {
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::Bool(*self)
	}
}

macro_rules! integer_parameter {
	($($ty:ty),+ $(,)?) => {
		$(
			impl StatusParameter for $ty {
				fn to_diagnostic_value(&self) -> DiagnosticValue {
					DiagnosticValue::Integer(*self as i64)
				}
			}
		)+
	};
}

macro_rules! unsigned_parameter {
	($($ty:ty),+ $(,)?) => {
		$(
			impl StatusParameter for $ty {
				fn to_diagnostic_value(&self) -> DiagnosticValue {
					DiagnosticValue::Unsigned(*self as u64)
				}
			}
		)+
	};
}

integer_parameter!(i8, i16, i32, i64);
unsigned_parameter!(u8, u16, u32, u64);

impl StatusParameter for f32 {
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::Float(f64::from(*self))
	}
}

impl StatusParameter for f64 {
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::Float(*self)
	}
}

impl<T: StatusParameter> StatusParameter for Vec<T> {
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::List(self.iter().map(StatusParameter::to_diagnostic_value).collect())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn escapes_identifiers_and_literals() {
		assert_eq!(Formatted::<Identifier, _>::new("a`b").to_string(), "`a``b`");
		assert_eq!(Formatted::<StringLiteral, _>::new("it's").to_string(), "'it''s'");
	}

	#[test]
	fn joins_zero_one_two_and_many_values() {
		let empty: Vec<String> = Vec::new();
		let one = vec!["a".to_owned()];
		let two = vec!["a".to_owned(), "b".to_owned()];
		let three = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];

		assert_eq!(Formatted::<Join<And, Identifier>, _>::new(&empty).to_string(), "");
		assert_eq!(Formatted::<Join<And, Identifier>, _>::new(&one).to_string(), "`a`");
		assert_eq!(Formatted::<Join<And, Identifier>, _>::new(&two).to_string(), "`a` and `b`");
		assert_eq!(
			Formatted::<Join<Or, Identifier>, _>::new(&three).to_string(),
			"`a`, `b`, or `c`"
		);
	}
}
