use std::fmt::{self, Display, Formatter};

use super::{AtomKind, GraphKind, Number, Text, Value, VectorKind};

/// Runtime value-type constraint, including collections, graph values and
/// callable signatures.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum Type {
	/// Type constraint accepting an unconstrained value.
	Any,
	/// Type constraint accepting an absent value.
	None,
	/// Type constraint accepting an explicit null value.
	Null,
	/// Type constraint accepting a Boolean value.
	Bool,
	/// Type constraint accepting a numeric value.
	Number,
	/// Type constraint accepting a signed integer.
	Int,
	/// Type constraint accepting a floating-point number.
	Float,
	/// Type constraint accepting a decimal number.
	Decimal,
	/// Type constraint accepting text.
	String,
	/// Type constraint accepting a duration.
	Duration,
	/// Type constraint accepting a time-zone-aware timestamp.
	DateTime,
	/// Type constraint accepting a ULID.
	Ulid,
	/// Type constraint accepting a UUID.
	Uuid,
	/// Type constraint accepting an ordered sequence of values.
	Array(
		/// Type constraint accepting an ordered sequence of values.
		Box<Type>,
	),
	/// Type constraint accepting an ordered set of distinct values.
	Set(
		/// Type constraint accepting an ordered set of distinct values.
		Box<Type>,
	),
	/// Type constraint accepting a text-keyed value map.
	Object,
	/// Type constraint accepting a geometry value.
	Geometry,
	/// Type constraint accepting a numeric vector.
	Vector(
		/// Type constraint accepting a numeric vector.
		VectorKind,
	),
	/// Type constraint accepting binary bytes.
	Bytes,
	/// Type constraint accepting a stable graph reference.
	GraphRef(
		/// Type constraint accepting a stable graph reference.
		GraphKind,
	),
	/// Type constraint accepting a class-qualified record identity.
	Record,
	/// Type constraint accepting a compiled regular expression.
	Regex,
	/// Type constraint accepting a runtime matcher.
	Matcher,
	/// Type constraint accepting a bounded or unbounded value interval.
	Range(
		/// Type constraint accepting a bounded or unbounded value interval.
		Box<Type>,
	),
	/// Type constraint accepting a stable atom reference.
	Atom(
		/// Type constraint accepting a stable atom reference.
		AtomKind,
	),
	/// Type constraint accepting a stable graph-qualified atom occurrence.
	AtomOccurrence(
		/// Type constraint accepting a stable graph-qualified atom occurrence.
		AtomKind,
	),
	/// Type constraint accepting a snapshot-qualified path.
	Path,
	/// Type constraint accepting a materialized graph family.
	Graph(
		/// Type constraint accepting a materialized graph family.
		GraphKind,
	),
	/// Type constraint accepting an optional value.
	Option(
		/// Type constraint accepting an optional value.
		Box<Type>,
	),
	/// Type constraint accepting a union of accepted value types.
	Union(
		/// Type constraint accepting a union of accepted value types.
		Vec<Type>,
	),
	/// Type constraint accepting a structured record constraint.
	RecordShape(
		/// Type constraint accepting a structured record constraint.
		Vec<(Text, Type)>,
	),
	/// Type constraint accepting a runtime callable.
	Closure {
		/// Declared parameters or bound values supplied to this operation.
		parameters: Vec<Type>,
		/// Declared result type of the callable signature.
		result: Box<Type>,
	},
	/// Type constraint accepting a runtime error value.
	Error,
	/// Type constraint accepting a runtime query cursor.
	Cursor,
}

impl Type {
	/// Visits borrowed nodes in declaration order, stopping on the caller's
	/// error.
	///
	/// # Errors
	///
	/// Propagates the visitor's resource or validation error before descending.
	pub(crate) fn try_visit<Error>(
		&self,
		depth: usize,
		visit: &mut impl FnMut(&Self, usize) -> Result<(), Error>,
	) -> Result<(), Error> {
		visit(self, depth)?;
		let depth = depth.saturating_add(1);
		match self {
			Self::Array(inner) | Self::Set(inner) | Self::Range(inner) | Self::Option(inner) => {
				inner.try_visit(depth, visit)?;
			}
			Self::Union(types) => {
				for value in types {
					value.try_visit(depth, visit)?;
				}
			}
			Self::RecordShape(fields) => {
				for (_, value) in fields {
					value.try_visit(depth, visit)?;
				}
			}
			Self::Closure {
				parameters,
				result,
			} => {
				for value in parameters {
					value.try_visit(depth, visit)?;
				}
				result.try_visit(depth, visit)?;
			}
			_ => {}
		}
		Ok(())
	}

	/// Estimates retained layout without imposing an embedding's type-depth
	/// policy.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		let mut bytes = 0_usize;
		let result = self.try_visit(0, &mut |value, _depth| {
			let owned = match value {
				Self::Array(_) | Self::Set(_) | Self::Range(_) | Self::Option(_) => {
					size_of::<Self>()
				}
				Self::Union(values) => values.capacity().saturating_mul(size_of::<Self>()),
				Self::RecordShape(fields) => fields.iter().fold(
					fields.capacity().saturating_mul(size_of::<(Text, Self)>()),
					|bytes, (name, _)| bytes.saturating_add(name.estimated_heap_bytes()),
				),
				Self::Closure {
					parameters,
					..
				} => parameters
					.capacity()
					.saturating_mul(size_of::<Self>())
					.saturating_add(size_of::<Self>()),
				_ => 0,
			};
			bytes = bytes.saturating_add(owned);
			Ok::<(), std::convert::Infallible>(())
		});
		match result {
			Ok(()) => bytes,
			Err(never) => match never {},
		}
	}

	/// Returns of for `Type`.
	pub fn of(value: &Value) -> Self {
		match value {
			Value::None => Self::None,
			Value::Null => Self::Null,
			Value::Bool(_) => Self::Bool,
			Value::Number(Number::Int(_)) => Self::Int,
			Value::Number(Number::Float(_)) => Self::Float,
			Value::Number(Number::Decimal(_)) => Self::Decimal,
			Value::String(_) => Self::String,
			Value::Duration(_) => Self::Duration,
			Value::DateTime(_) => Self::DateTime,
			Value::Ulid(_) => Self::Ulid,
			Value::Uuid(_) => Self::Uuid,
			Value::Array(_) => Self::Array(Box::new(Self::Any)),
			Value::Set(_) => Self::Set(Box::new(Self::Any)),
			Value::Object(_) => Self::Object,
			Value::Geometry(_) => Self::Geometry,
			Value::Vector(value) => Self::Vector(value.kind()),
			Value::Bytes(_) => Self::Bytes,
			Value::GraphRef(value) => Self::GraphRef(value.kind()),
			Value::Record(_) => Self::Record,
			Value::Regex(_) => Self::Regex,
			Value::Matcher(_) => Self::Matcher,
			Value::Range(_) => Self::Range(Box::new(Self::Any)),
			Value::Atom(value) => Self::Atom(value.kind()),
			Value::AtomOccurrence(value) => Self::AtomOccurrence(value.atom().kind()),
			Value::Path(_) => Self::Path,
			Value::Graph(value) => Self::Graph(value.kind()),
			Value::Closure(value) => value.signature(),
			Value::Error(_) => Self::Error,
			Value::Cursor(_) => Self::Cursor,
		}
	}
}

impl Display for Type {
	/// Formats `Type` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::Any => f.write_str("any"),
			Self::None => f.write_str("none"),
			Self::Null => f.write_str("null"),
			Self::Bool => f.write_str("bool"),
			Self::Number => f.write_str("number"),
			Self::Int => f.write_str("int"),
			Self::Float => f.write_str("float"),
			Self::Decimal => f.write_str("decimal"),
			Self::String => f.write_str("string"),
			Self::Duration => f.write_str("duration"),
			Self::DateTime => f.write_str("datetime"),
			Self::Ulid => f.write_str("ulid"),
			Self::Uuid => f.write_str("uuid"),
			Self::Array(value) => write!(f, "array<{value}>"),
			Self::Set(value) => write!(f, "set<{value}>"),
			Self::Object => f.write_str("object"),
			Self::Geometry => f.write_str("geometry"),
			Self::Vector(kind) => write!(f, "vector<{kind:?}>"),
			Self::Bytes => f.write_str("bytes"),
			Self::GraphRef(kind) => write!(f, "graph_ref<{kind}>"),
			Self::Record => f.write_str("record"),
			Self::Regex => f.write_str("regex"),
			Self::Matcher => f.write_str("matcher"),
			Self::Range(value) => write!(f, "range<{value}>"),
			Self::Atom(kind) => write!(f, "atom<{kind}>"),
			Self::AtomOccurrence(kind) => write!(f, "atom_occurrence<{kind}>"),
			Self::Path => f.write_str("path"),
			Self::Graph(kind) => write!(f, "graph<{kind}>"),
			Self::Option(value) => write!(f, "option<{value}>"),
			Self::Union(values) => {
				for (index, value) in values.iter().enumerate() {
					if index > 0 {
						f.write_str(" | ")?;
					}
					Display::fmt(value, f)?;
				}
				Ok(())
			}
			Self::RecordShape(fields) => {
				f.write_str("record<{")?;
				for (index, (name, value)) in fields.iter().enumerate() {
					if index > 0 {
						f.write_str(", ")?;
					}
					write!(f, "{name}: {value}")?;
				}
				f.write_str("}>")
			}
			Self::Closure {
				parameters,
				result,
			} => {
				f.write_str("closure<(")?;
				for (index, parameter) in parameters.iter().enumerate() {
					if index > 0 {
						f.write_str(", ")?;
					}
					Display::fmt(parameter, f)?;
				}
				write!(f, ") -> {result}>")
			}
			Self::Error => f.write_str("error"),
			Self::Cursor => f.write_str("cursor"),
		}
	}
}

/// Operations shared with the engine and value codecs.
#[allow(
	clippy::impl_trait_in_params,
	reason = "Preserves existing internal operation signatures across extraction"
)]
pub trait TypeInternal: Sized {
	/// Visits borrowed nodes in declaration order, stopping on the caller's
	/// error.
	///
	/// # Errors
	///
	/// Propagates the visitor's resource or validation error before descending.
	fn try_visit<Error>(
		&self,
		depth: usize,
		visit: &mut impl FnMut(&Self, usize) -> Result<(), Error>,
	) -> Result<(), Error>;
	/// Estimates retained layout without imposing an embedding's type-depth
	/// policy.
	fn estimated_heap_bytes(&self) -> usize;
}
impl TypeInternal for Type {
	fn try_visit<Error>(
		&self,
		depth: usize,
		visit: &mut impl FnMut(&Self, usize) -> Result<(), Error>,
	) -> Result<(), Error> {
		<Type>::try_visit::<Error>(self, depth, visit)
	}

	fn estimated_heap_bytes(&self) -> usize {
		<Type>::estimated_heap_bytes(self)
	}
}

#[cfg(test)]
/// Regression tests for types behavior.
mod tests {
	use super::Type;

	/// Verifies option is static only.
	#[test]
	fn option_is_static_only() {
		let value_type = Type::Option(Box::new(Type::String));
		assert_eq!(value_type.to_string(), "option<string>");
	}

	/// Verifies formats nested types.
	#[test]
	fn formats_nested_types() {
		let value_type = Type::Array(Box::new(Type::Set(Box::new(Type::Int))));
		assert_eq!(value_type.to_string(), "array<set<int>>");
	}
}
