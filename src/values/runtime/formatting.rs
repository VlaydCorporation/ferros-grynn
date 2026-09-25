use std::fmt::{self, Display, Formatter};

use super::{Value, Vector};

impl Display for Value {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::None => f.write_str("none"),
			Self::Null => f.write_str("null"),
			Self::Bool(value) => Display::fmt(value, f),
			Self::Number(value) => Display::fmt(value, f),
			Self::String(value) => write_json_string(f, value),
			Self::Duration(value) => Display::fmt(value, f),
			Self::DateTime(value) => Display::fmt(value, f),
			Self::Ulid(value) => Display::fmt(value, f),
			Self::Uuid(value) => Display::fmt(value, f),
			Self::Array(value) => write_sequence(f, "[", "]", value.iter()),
			Self::Set(value) => write_sequence(f, "set[", "]", value.iter()),
			Self::Object(value) => {
				f.write_str("{")?;
				for (index, (key, value)) in value.iter().enumerate() {
					if index > 0 {
						f.write_str(", ")?;
					}
					write_json_string(f, key)?;
					write!(f, ": {value}")?;
				}
				f.write_str("}")
			}
			Self::Geometry(value) => Display::fmt(value, f),
			Self::Vector(value) => match value {
				Vector::F32(values) => write_debug_sequence(f, "vector<f32>", values),
				Vector::F64(values) => write_debug_sequence(f, "vector<f64>", values),
				Vector::I16(values) => write_debug_sequence(f, "vector<i16>", values),
				Vector::I32(values) => write_debug_sequence(f, "vector<i32>", values),
				Vector::I64(values) => write_debug_sequence(f, "vector<i64>", values),
			},
			Self::Bytes(value) => {
				f.write_str("b\"")?;
				for byte in value {
					write!(f, "{byte:02x}")?;
				}
				f.write_str("\"")
			}
			Self::Record(value) => Display::fmt(value, f),
			Self::GraphRef(value) => write!(f, "graph_ref({}#{})", value.name(), value.id()),
			Self::Regex(value) => Display::fmt(value, f),
			Self::Range(value) => Display::fmt(value, f),
			Self::Atom(value) => Display::fmt(value.id(), f),
			Self::Path(value) => write!(
				f,
				"path({}@{}; length={})",
				value.origin(),
				value.snapshot(),
				value.length(),
			),
			Self::Graph(value) => {
				write!(f, "{}({}@{})", value.kind().name(), value.identity(), value.revision(),)
			}
			Self::Closure(value) => write!(
				f,
				"<closure:{}#{}>",
				value.name_str().unwrap_or("anonymous"),
				value.id().get(),
			),
			Self::Error(value) => write!(f, "error({}: {})", value.code(), value.message()),
			Self::Cursor(value) => Display::fmt(value, f),
		}
	}
}

fn write_json_string(formatter: &mut Formatter<'_>, value: &str) -> fmt::Result {
	let value = serde_json::to_string(value).map_err(|_err| fmt::Error)?;
	formatter.write_str(&value)
}

fn write_sequence<'a>(
	formatter: &mut Formatter<'_>,
	prefix: &str,
	suffix: &str,
	values: impl IntoIterator<Item = &'a Value>,
) -> fmt::Result {
	formatter.write_str(prefix)?;
	for (index, value) in values.into_iter().enumerate() {
		if index > 0 {
			formatter.write_str(", ")?;
		}
		Display::fmt(value, formatter)?;
	}
	formatter.write_str(suffix)
}

fn write_debug_sequence<T: fmt::Debug>(
	formatter: &mut Formatter<'_>,
	prefix: &str,
	values: &[T],
) -> fmt::Result {
	write!(formatter, "{prefix}{values:?}")
}
