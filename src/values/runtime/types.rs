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
	GraphRef(GraphKind),
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
	Error,
	Cursor,
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
			Value::GraphRef(value) => Self::GraphRef(value.kind()),
			Value::Record(_) => Self::Record,
			Value::Regex(_) => Self::Regex,
			Value::Range(_) => Self::Range(Box::new(Self::Any)),
			Value::Atom(value) => Self::Atom(value.kind()),
			Value::Path(_) => Self::Path,
			Value::Graph(value) => Self::Graph(value.kind()),
			Value::Closure(value) => value.signature(),
			Value::Error(_) => Self::Error,
			Value::Cursor(_) => Self::Cursor,
		}
	}
}

impl Display for Type {
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
			Self::Range(value) => write!(f, "range<{value}>"),
			Self::Atom(kind) => write!(f, "atom<{kind}>"),
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
