use std::{
	fmt::{self, Display, Formatter},
	marker::PhantomData,
};

use serde::{Deserialize, Serialize};

/// Formats the status parameter without intermediate `String` allocation.
pub trait ParameterFormatter<T: ?Sized> {
	/// Formats the value using the corresponding text representation.
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result;
}

/// Adapter of the parameter to the standard [`Display`].
pub struct Formatted<'a, F, T: ?Sized> {
	/// The value to which formatting or a status report is applied.
	value: &'a T,
	/// A marker for the selected parameter-formatting method.
	formatter: PhantomData<F>,
}

impl<'a, F, T: ?Sized> Formatted<'a, F, T> {
	/// Creates a value from the supplied inputs.
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
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		F::fmt(self.value, f)
	}
}

/// A marker for formatting a parameter using its standard text
/// representation.
#[non_exhaustive]
pub struct DisplayValue;
/// A marker for formatting an identifier with backtick escaping.
#[non_exhaustive]
pub struct Identifier;
/// A marker for formatting a string literal with single-quote
/// escaping.
#[non_exhaustive]
pub struct StringLiteral;
/// A marker for formatting a callable name with trailing parentheses.
#[non_exhaustive]
pub struct Callable;
/// A marker for formatting a query parameter with a dollar-sign prefix.
#[non_exhaustive]
pub struct QueryParameter;
/// A marker for a value type's text representation in a diagnostic message.
#[non_exhaustive]
pub struct ValueType;

impl<T: Display + ?Sized> ParameterFormatter<T> for DisplayValue {
	/// Formats the value using the corresponding text representation.
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(values, formatter)
	}
}

/// Writes a string with the escaping required by the selected representation.
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
	/// Formats the value using the corresponding text representation.
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		write_escaped(&values.to_string(), formatter, '`')
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for StringLiteral {
	/// Formats the value using the corresponding text representation.
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		write_escaped(&values.to_string(), formatter, '\'')
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for Callable {
	/// Formats the value using the corresponding text representation.
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		write!(formatter, "{values}()")
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for QueryParameter {
	/// Formats the value using the corresponding text representation.
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str("$")?;
		Identifier::fmt(values, formatter)
	}
}

impl<T: Display + ?Sized> ParameterFormatter<T> for ValueType {
	/// Formats the value using the corresponding text representation.
	fn fmt(values: &T, formatter: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(values, formatter)
	}
}

/// Rules for writing separators between items and before the final item of a
/// list.
pub trait JoinSeparator {
	/// Writes the separator between adjacent list items.
	fn write_between(formatter: &mut Formatter<'_>) -> fmt::Result;
	/// Writes the separator before the final list item.
	fn write_last(formatter: &mut Formatter<'_>, has_multiple_prefix_items: bool) -> fmt::Result;
}

/// Comma separation of formatted items.
#[non_exhaustive]
pub struct Comma;
/// Comma separation with `and` before the final item.
#[non_exhaustive]
pub struct And;
/// Comma separation with `or` before the final item.
#[non_exhaustive]
pub struct Or;

impl JoinSeparator for Comma {
	/// Writes the separator between adjacent list items.
	fn write_between(formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(", ")
	}

	/// Writes the separator before the final list item.
	fn write_last(formatter: &mut Formatter<'_>, _has_multiple_prefix_items: bool) -> fmt::Result {
		formatter.write_str(", ")
	}
}

impl JoinSeparator for And {
	/// Writes the separator between adjacent list items.
	fn write_between(formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(", ")
	}

	/// Writes the separator before the final list item.
	fn write_last(formatter: &mut Formatter<'_>, has_multiple_prefix_items: bool) -> fmt::Result {
		if has_multiple_prefix_items {
			formatter.write_str(", and ")
		} else {
			formatter.write_str(" and ")
		}
	}
}

impl JoinSeparator for Or {
	/// Writes the separator between adjacent list items.
	fn write_between(formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(", ")
	}

	/// Writes the separator before the final list item.
	fn write_last(formatter: &mut Formatter<'_>, has_multiple_prefix_items: bool) -> fmt::Result {
		if has_multiple_prefix_items {
			formatter.write_str(", or ")
		} else {
			formatter.write_str(" or ")
		}
	}
}

/// A marker for formatting a list with a selected separator and a format for
/// each item.
pub struct Join<S, F>(
	/// A marker for the separator and item-formatting types.
	PhantomData<(S, F)>,
);

impl<S, F, T> ParameterFormatter<[T]> for Join<S, F>
where
	S: JoinSeparator,
	F: ParameterFormatter<T>,
{
	/// Formats the value using the corresponding text representation.
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
	/// Formats the value using the corresponding text representation.
	fn fmt(values: &Vec<T>, formatter: &mut Formatter<'_>) -> fmt::Result {
		<Self as ParameterFormatter<[T]>>::fmt(values, formatter)
	}
}

/// The format is an independent parameter value for wire DTO diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum DiagnosticValue {
	/// An explicit diagnostic null value.
	Null,
	/// A boolean parameter value.
	Bool(
		/// A boolean parameter value.
		bool,
	),
	/// A signed integer parameter value.
	Integer(
		/// A signed integer parameter value.
		i64,
	),
	/// An unsigned integer parameter value.
	Unsigned(
		/// An unsigned integer parameter value.
		u64,
	),
	/// A floating-point parameter value.
	Float(
		/// A floating-point parameter value.
		f64,
	),
	/// A string parameter value.
	String(
		/// A string parameter value.
		String,
	),
	/// An ordered list of diagnostic values.
	List(
		/// An ordered list of diagnostic values.
		Vec<DiagnosticValue>,
	),
	/// Named diagnostic values ordered by key.
	Map(
		/// Named diagnostic values ordered by key.
		std::collections::BTreeMap<String, DiagnosticValue>,
	),
}

/// Converts a typed status parameter to a value for inspection.
pub trait StatusParameter {
	/// Copies a parameter into a structured diagnostic representation.
	fn to_diagnostic_value(&self) -> DiagnosticValue;
}

/// Exact owned parameter recovery used by generated status constructors.
pub(crate) trait CapturedParameter: Sized {
	/// Admits parameter clones before producing a historical diagnostic map.
	fn allocation_bytes(&self) -> usize;
	/// Rejects a mismatched diagnostic kind rather than coercing it.
	fn from_diagnostic(value: DiagnosticValue) -> Option<Self>;
}

impl CapturedParameter for String {
	/// Estimates the allocated memory for a string parameter.
	fn allocation_bytes(&self) -> usize {
		self.len()
	}

	/// Validates and restores a string parameter from a diagnostic
	/// value.
	fn from_diagnostic(value: DiagnosticValue) -> Option<Self> {
		if let DiagnosticValue::String(value) = value {
			Some(value)
		} else {
			None
		}
	}
}

/// Primitive status fields preserve their declared numeric representation.
macro_rules! captured_primitive {
	($type:ty, $variant:ident) => {
		impl CapturedParameter for $type {
			/// Returns the memory needed to copy a scalar
			/// parameter.
			fn allocation_bytes(&self) -> usize {
				0
			}

			/// Validates the diagnostic value kind and restores the typed
			/// parameter.
			fn from_diagnostic(value: DiagnosticValue) -> Option<Self> {
				if let DiagnosticValue::$variant(value) = value {
					Some(value)
				} else {
					None
				}
			}
		}
	};
}

captured_primitive!(u64, Unsigned);
captured_primitive!(i64, Integer);
captured_primitive!(f64, Float);
captured_primitive!(bool, Bool);

impl StatusParameter for String {
	/// Copies a parameter into a structured diagnostic representation.
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::String(self.clone())
	}
}

impl StatusParameter for str {
	/// Copies a parameter into a structured diagnostic representation.
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::String(self.to_owned())
	}
}

impl StatusParameter for bool {
	/// Copies a parameter into a structured diagnostic representation.
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::Bool(*self)
	}
}

/// Implements capture of signed integer parameters into a diagnostic
/// value.
macro_rules! integer_parameter {
	($($ty:ty),+ $(,)?) => {
		$(
			impl StatusParameter for $ty {
				/// Copies a parameter into a structured diagnostic representation.
				fn to_diagnostic_value(&self) -> DiagnosticValue {
					DiagnosticValue::Integer(*self as i64)
				}
			}
		)+
	};
}

/// Implements capture of unsigned integer parameters into a diagnostic
/// value.
macro_rules! unsigned_parameter {
	($($ty:ty),+ $(,)?) => {
		$(
			impl StatusParameter for $ty {
				/// Copies a parameter into a structured diagnostic representation.
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
	/// Copies a parameter into a structured diagnostic representation.
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::Float(f64::from(*self))
	}
}

impl StatusParameter for f64 {
	/// Copies a parameter into a structured diagnostic representation.
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::Float(*self)
	}
}

impl<T: StatusParameter> StatusParameter for Vec<T> {
	/// Copies a parameter into a structured diagnostic representation.
	fn to_diagnostic_value(&self) -> DiagnosticValue {
		DiagnosticValue::List(self.iter().map(StatusParameter::to_diagnostic_value).collect())
	}
}

#[cfg(test)]
/// Regression tests for formatting behavior.
mod tests {
	use super::*;

	/// Tests the `escapes_identifiers_and_literals` scenario and its
	/// diagnostic outcome.
	#[test]
	fn escapes_identifiers_and_literals() {
		assert_eq!(Formatted::<Identifier, _>::new("a`b").to_string(), "`a``b`");
		assert_eq!(Formatted::<StringLiteral, _>::new("it's").to_string(), "'it''s'");
	}

	/// Tests the `joins_zero_one_two_and_many_values` scenario and its
	/// diagnostic outcome.
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
