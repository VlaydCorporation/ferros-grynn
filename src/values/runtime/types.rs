use std::fmt::{self, Display, Formatter};

use super::{AtomKind, GraphKind, Number, Text, Value, VectorKind};

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum Type {
	Any,
	None,
	Null,
	Bool,
	Number,
	Int,
	Float,
	Decimal,
	String,
	Duration,
	DateTime,
	Ulid,
	Uuid,
	Array(Box<Type>),
	Set(Box<Type>),
	Object,
	Geometry,
	Vector(VectorKind),
	Bytes,
	Record,
	Regex,
	Range(Box<Type>),
	Atom(AtomKind),
	Path,
	Graph(GraphKind),
	Option(Box<Type>),
	Union(Vec<Type>),
	RecordShape(Vec<(Text, Type)>),
	Closure {
		parameters: Vec<Type>,
		result: Box<Type>,
	},
}

impl Type {
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
			Value::Record(_) => Self::Record,
			Value::Regex(_) => Self::Regex,
			Value::Range(_) => Self::Range(Box::new(Self::Any)),
			Value::Atom(value) => Self::Atom(value.kind()),
			Value::Path(_) => Self::Path,
			Value::Graph(value) => Self::Graph(value.kind()),
			Value::Closure(value) => value.signature(),
		}
	}
}

impl Display for Type {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::Any => formatter.write_str("any"),
			Self::None => formatter.write_str("none"),
			Self::Null => formatter.write_str("null"),
			Self::Bool => formatter.write_str("bool"),
			Self::Number => formatter.write_str("number"),
			Self::Int => formatter.write_str("int"),
			Self::Float => formatter.write_str("float"),
			Self::Decimal => formatter.write_str("decimal"),
			Self::String => formatter.write_str("string"),
			Self::Duration => formatter.write_str("duration"),
			Self::DateTime => formatter.write_str("datetime"),
			Self::Ulid => formatter.write_str("ulid"),
			Self::Uuid => formatter.write_str("uuid"),
			Self::Array(value) => write!(formatter, "array<{value}>"),
			Self::Set(value) => write!(formatter, "set<{value}>"),
			Self::Object => formatter.write_str("object"),
			Self::Geometry => formatter.write_str("geometry"),
			Self::Vector(kind) => write!(formatter, "vector<{kind:?}>"),
			Self::Bytes => formatter.write_str("bytes"),
			Self::Record => formatter.write_str("record"),
			Self::Regex => formatter.write_str("regex"),
			Self::Range(value) => write!(formatter, "range<{value}>"),
			Self::Atom(kind) => write!(formatter, "atom<{kind}>"),
			Self::Path => formatter.write_str("path"),
			Self::Graph(kind) => write!(formatter, "graph<{kind}>"),
			Self::Option(value) => write!(formatter, "option<{value}>"),
			Self::Union(values) => {
				for (index, value) in values.iter().enumerate() {
					if index > 0 {
						formatter.write_str(" | ")?;
					}
					Display::fmt(value, formatter)?;
				}
				Ok(())
			}
			Self::RecordShape(fields) => {
				formatter.write_str("record<{")?;
				for (index, (name, value)) in fields.iter().enumerate() {
					if index > 0 {
						formatter.write_str(", ")?;
					}
					write!(formatter, "{name}: {value}")?;
				}
				formatter.write_str("}>")
			}
			Self::Closure {
				parameters,
				result,
			} => {
				formatter.write_str("closure<(")?;
				for (index, parameter) in parameters.iter().enumerate() {
					if index > 0 {
						formatter.write_str(", ")?;
					}
					Display::fmt(parameter, formatter)?;
				}
				write!(formatter, ") -> {result}>")
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::Type;

	#[test]
	fn option_is_static_only() {
		let value_type = Type::Option(Box::new(Type::String));
		assert_eq!(value_type.to_string(), "option<string>");
	}

	#[test]
	fn formats_nested_types() {
		let value_type = Type::Array(Box::new(Type::Set(Box::new(Type::Int))));
		assert_eq!(value_type.to_string(), "array<set<int>>");
	}
}
