use std::{
	fmt::{self, Display, Formatter},
	marker::PhantomData,
};

use serde::{Deserialize, Serialize};

/// Formats the status parameter without intermediate `String` allocation.
pub trait ParameterFormatter<T: ?Sized> {
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result;
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

#[non_exhaustive]
pub struct DisplayValue;
#[non_exhaustive]
pub struct Identifier;
#[non_exhaustive]
pub struct StringLiteral;
#[non_exhaustive]
pub struct Callable;
#[non_exhaustive]
pub struct QueryParameter;
#[non_exhaustive]
pub struct ValueType;

impl<T: Display + ?Sized> ParameterFormatter<T> for DisplayValue {
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(values, formatter)
	}
}

fn write_escaped(value: &str, formatter: &mut Formatter<'_>, delimiter: char) -> fmt::Result {
	write!(formatter, "{delimiter}")?;
	for character in value.chars() {
		if character == delimiter {
			write!(formatter, "{delimiter}{delimiter}")?;
		} else {
			write!(formatter, "{character}")?;
		}
	}
	write!(formatter, "{delimiter}")
}

impl<T: Display + ?Sized> ParameterFormatter<T> for Identifier {
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		write_escaped(&values.to_string(), formatter, '`')
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for StringLiteral {
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		write_escaped(&values.to_string(), formatter, '\'')
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for Callable {
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		write!(formatter, "{values}()")
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for QueryParameter {
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str("$")?;
		Identifier::fmt(values, formatter)
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for ValueType {
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(values, formatter)
	}
}

pub trait JoinSeparator {
	fn write_between(formatter: &mut Formatter<'_>) -> fmt::Result;
	fn write_last(formatter: &mut Formatter<'_>, has_multiple_prefix_items: bool) -> fmt::Result;
}

#[non_exhaustive]
pub struct Comma;
#[non_exhaustive]
pub struct And;
#[non_exhaustive]
pub struct Or;

impl JoinSeparator for Comma {
	fn write_between(formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(", ")
	}

	fn write_last(formatter: &mut Formatter<'_>, _has_multiple_prefix_items: bool) -> fmt::Result {
		formatter.write_str(", ")
	}
}

impl JoinSeparator for And {
	fn write_between(formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(", ")
	}

	fn write_last(formatter: &mut Formatter<'_>, has_multiple_prefix_items: bool) -> fmt::Result {
		if has_multiple_prefix_items {
			formatter.write_str(", and ")
		} else {
			formatter.write_str(" and ")
		}
	}
}

impl JoinSeparator for Or {
	fn write_between(formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(", ")
	}

	fn write_last(formatter: &mut Formatter<'_>, has_multiple_prefix_items: bool) -> fmt::Result {
		if has_multiple_prefix_items {
			formatter.write_str(", or ")
		} else {
			formatter.write_str(" or ")
		}
	}
}

pub struct Join<S, F>(PhantomData<(S, F)>);

impl<S, F, T> ParameterFormatter<[T]> for Join<S, F>
where
	S: JoinSeparator,
	F: ParameterFormatter<T>,
{
	fn fmt(values: &[T], formatter: &mut Formatter<'_>) -> fmt::Result {
		for (index, value) in values.iter().enumerate() {
			if index > 0 {
				if index + 1 == values.len() {
					S::write_last(formatter, values.len() > 2)?;
				} else {
					S::write_between(formatter)?;
				}
			}
			F::fmt(value, formatter)?;
		}
		Ok(())
	}
}

impl<S, F, T> ParameterFormatter<Vec<T>> for Join<S, F>
where
	S: JoinSeparator,
	F: ParameterFormatter<T>,
{
	fn fmt(values: &Vec<T>, formatter: &mut Formatter<'_>) -> fmt::Result {
		<Self as ParameterFormatter<[T]>>::fmt(values, formatter)
	}
}

/// The format is an independent parameter value for wire DTO diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
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
