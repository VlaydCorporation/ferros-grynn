mod array;
mod bytes;
mod closure;
mod geometry;
pub mod graph;
mod id;
mod number;
mod object;
mod property_map;
mod range;
mod record_id;
mod regex;
mod set;
mod temporal;
mod text;
mod types;
mod vector;

#[cfg(test)]
mod laws;

use std::{
	cmp::Ordering,
	fmt::{self, Display, Formatter},
	hash::{Hash, Hasher},
};

pub use array::Array;
pub use bytes::Bytes;
pub use closure::{Closure, RuntimeId};
pub use geometry::{Geometry, Position};
pub use graph::{
	Frozen,
	FrozenGraph,
	Graph,
	GraphKind,
	GraphRef,
	GraphStorageMode,
	Mutable,
	MutableGraph,
};
pub use id::{Ulid, Uuid};
pub use number::Number;
pub use object::Object;
pub use property_map::{PropertyIter, PropertyKeyId, PropertyMap};
pub use range::Range;
pub use record_id::{RecordId, RecordIdPart};
pub use regex::Regex;
pub use set::Set;
pub use temporal::{DateTime, Duration};
pub use text::Text;
pub use types::Type;
pub use vector::{Vector, VectorKind};

use crate::gql_status::FerrosGrynnError;

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum ValueKind {
	None,
	Null,
	Bool,
	Int,
	Float,
	Decimal,
	String,
	Duration,
	DateTime,
	Ulid,
	Uuid,
	Array,
	Set,
	Object,
	Geometry,
	Vector(VectorKind),
	Bytes,
	Record,
	Regex,
	Range,
	Graph,
	Closure,
}

impl ValueKind {
	pub const fn stable_rank(self) -> u8 {
		match self {
			Self::None => 0,
			Self::Null => 1,
			Self::Bool => 2,
			Self::Int | Self::Float | Self::Decimal => 3,
			Self::String => 4,
			Self::Duration => 5,
			Self::DateTime => 6,
			Self::Ulid => 7,
			Self::Uuid => 8,
			Self::Array => 9,
			Self::Set => 10,
			Self::Object => 11,
			Self::Geometry => 12,
			Self::Vector(_) => 13,
			Self::Bytes => 14,
			// Rank 15 is reserved for the future table-reference runtime value.
			Self::Record => 16,
			Self::Regex => 17,
			Self::Range => 18,
			Self::Graph => 22,
			Self::Closure => 23,
		}
	}

	pub const fn name(self) -> &'static str {
		match self {
			Self::None => "none",
			Self::Null => "null",
			Self::Bool => "bool",
			Self::Int => "int",
			Self::Float => "float",
			Self::Decimal => "decimal",
			Self::String => "string",
			Self::Duration => "duration",
			Self::DateTime => "datetime",
			Self::Ulid => "ulid",
			Self::Uuid => "uuid",
			Self::Array => "array",
			Self::Set => "set",
			Self::Object => "object",
			Self::Geometry => "geometry",
			Self::Vector(_) => "vector",
			Self::Bytes => "bytes",
			Self::Record => "record",
			Self::Regex => "regex",
			Self::Range => "range",
			Self::Graph => "graph",
			Self::Closure => "closure",
		}
	}

	pub const fn is_number(self) -> bool {
		matches!(self, Self::Int | Self::Float | Self::Decimal)
	}

	pub fn is_compatible_with(self, other: Self) -> bool {
		(self.is_number() && other.is_number()) || self == other
	}
}

impl Display for ValueKind {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(self.name())
	}
}

#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub enum Value {
	#[default]
	None,
	Null,
	Bool(bool),
	Number(Number),
	String(Text),
	Duration(Duration),
	DateTime(DateTime),
	Ulid(Ulid),
	Uuid(Uuid),
	Array(Array),
	Set(Set),
	Object(Object),
	Geometry(Geometry),
	Vector(Vector),
	Bytes(Bytes),
	Record(Box<RecordId>),
	Regex(Regex),
	Range(Range),
	Graph(Box<Graph<Frozen>>),
	Closure(Box<Closure>),
}

impl Value {
	pub const fn kind(&self) -> ValueKind {
		match self {
			Self::None => ValueKind::None,
			Self::Null => ValueKind::Null,
			Self::Bool(_) => ValueKind::Bool,
			Self::Number(Number::Int(_)) => ValueKind::Int,
			Self::Number(Number::Float(_)) => ValueKind::Float,
			Self::Number(Number::Decimal(_)) => ValueKind::Decimal,
			Self::String(_) => ValueKind::String,
			Self::Duration(_) => ValueKind::Duration,
			Self::DateTime(_) => ValueKind::DateTime,
			Self::Ulid(_) => ValueKind::Ulid,
			Self::Uuid(_) => ValueKind::Uuid,
			Self::Array(_) => ValueKind::Array,
			Self::Set(_) => ValueKind::Set,
			Self::Object(_) => ValueKind::Object,
			Self::Geometry(_) => ValueKind::Geometry,
			Self::Vector(value) => ValueKind::Vector(value.kind()),
			Self::Bytes(_) => ValueKind::Bytes,
			Self::Record(_) => ValueKind::Record,
			Self::Regex(_) => ValueKind::Regex,
			Self::Range(_) => ValueKind::Range,
			Self::Graph(_) => ValueKind::Graph,
			Self::Closure(_) => ValueKind::Closure,
		}
	}

	pub fn value_type(&self) -> Type {
		Type::of(self)
	}

	pub const fn is_none(&self) -> bool {
		matches!(self, Self::None)
	}

	pub const fn is_null(&self) -> bool {
		matches!(self, Self::Null)
	}

	pub fn is_truthy(&self) -> bool {
		match self {
			Self::None | Self::Null => false,
			Self::Bool(value) => *value,
			Self::Number(value) => !value.is_zero(),
			Self::String(value) => !value.is_empty(),
			Self::Array(value) => !value.is_empty(),
			Self::Set(value) => !value.is_empty(),
			Self::Object(value) => !value.is_empty(),
			Self::Vector(value) => !value.is_empty(),
			Self::Bytes(value) => !value.is_empty(),
			Self::Duration(_)
			| Self::DateTime(_)
			| Self::Ulid(_)
			| Self::Uuid(_)
			| Self::Geometry(_)
			| Self::Record(_)
			| Self::Regex(_)
			| Self::Range(_)
			| Self::Graph(_)
			| Self::Closure(_) => true,
		}
	}

	pub const fn as_bool(&self) -> Option<bool> {
		match self {
			Self::Bool(value) => Some(*value),
			_ => None,
		}
	}

	pub const fn as_number(&self) -> Option<Number> {
		match self {
			Self::Number(value) => Some(*value),
			_ => None,
		}
	}

	pub fn as_str(&self) -> Option<&str> {
		self.as_text().map(Text::as_str)
	}

	pub const fn as_text(&self) -> Option<&Text> {
		match self {
			Self::String(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_array(&self) -> Option<&Array> {
		match self {
			Self::Array(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_set(&self) -> Option<&Set> {
		match self {
			Self::Set(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_object(&self) -> Option<&Object> {
		match self {
			Self::Object(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_duration(&self) -> Option<&Duration> {
		match self {
			Self::Duration(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_datetime(&self) -> Option<&DateTime> {
		match self {
			Self::DateTime(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_ulid(&self) -> Option<&Ulid> {
		match self {
			Self::Ulid(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_uuid(&self) -> Option<&Uuid> {
		match self {
			Self::Uuid(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_geometry(&self) -> Option<&Geometry> {
		match self {
			Self::Geometry(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_vector(&self) -> Option<&Vector> {
		match self {
			Self::Vector(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_bytes(&self) -> Option<&Bytes> {
		match self {
			Self::Bytes(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_record(&self) -> Option<&RecordId> {
		match self {
			Self::Record(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_regex(&self) -> Option<&Regex> {
		match self {
			Self::Regex(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_range(&self) -> Option<&Range> {
		match self {
			Self::Range(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_graph(&self) -> Option<&Graph<Frozen>> {
		match self {
			Self::Graph(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_closure(&self) -> Option<&Closure> {
		match self {
			Self::Closure(value) => Some(value),
			_ => None,
		}
	}

	pub fn is_storable(&self) -> bool {
		match self {
			Self::Array(values) => values.iter().all(Self::is_storable),
			Self::Set(values) => values.iter().all(Self::is_storable),
			Self::Object(values) => values.values().all(Self::is_storable),
			Self::Range(value) => {
				let start_storable = match value.start() {
					std::ops::Bound::Included(value) | std::ops::Bound::Excluded(value) => {
						value.is_storable()
					}
					std::ops::Bound::Unbounded => true,
				};
				let end_storable = match value.end() {
					std::ops::Bound::Included(value) | std::ops::Bound::Excluded(value) => {
						value.is_storable()
					}
					std::ops::Bound::Unbounded => true,
				};
				start_storable && end_storable
			}
			Self::Regex(_) | Self::Graph(_) | Self::Closure(_) => false,
			_ => true,
		}
	}

	pub fn ensure_storable(&self) -> Result<(), FerrosGrynnError> {
		self.is_storable()
			.then_some(())
			.ok_or_else(|| FerrosGrynnError::conversion_error(self.kind().name(), "StorageValue"))
	}
}

impl PartialEq for Value {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for Value {}

impl PartialOrd for Value {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Value {
	fn cmp(&self, other: &Self) -> Ordering {
		self.kind().stable_rank().cmp(&other.kind().stable_rank()).then_with(|| {
			match (self, other) {
				(Self::None, Self::None) | (Self::Null, Self::Null) => Ordering::Equal,
				(Self::Bool(left), Self::Bool(right)) => left.cmp(right),
				(Self::Number(left), Self::Number(right)) => left.cmp(right),
				(Self::String(left), Self::String(right)) => left.cmp(right),
				(Self::Duration(left), Self::Duration(right)) => left.cmp(right),
				(Self::DateTime(left), Self::DateTime(right)) => left.cmp(right),
				(Self::Ulid(left), Self::Ulid(right)) => left.cmp(right),
				(Self::Uuid(left), Self::Uuid(right)) => left.cmp(right),
				(Self::Array(left), Self::Array(right)) => left.cmp(right),
				(Self::Set(left), Self::Set(right)) => left.cmp(right),
				(Self::Object(left), Self::Object(right)) => left.cmp(right),
				(Self::Geometry(left), Self::Geometry(right)) => left.cmp(right),
				(Self::Vector(left), Self::Vector(right)) => left.cmp(right),
				(Self::Bytes(left), Self::Bytes(right)) => left.cmp(right),
				(Self::Record(left), Self::Record(right)) => left.cmp(right),
				(Self::Regex(left), Self::Regex(right)) => left.cmp(right),
				(Self::Range(left), Self::Range(right)) => left.cmp(right),
				(Self::Graph(left), Self::Graph(right)) => left.cmp(right),
				(Self::Closure(left), Self::Closure(right)) => left.cmp(right),
				_ => Ordering::Equal,
			}
		})
	}
}

impl Hash for Value {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.kind().stable_rank().hash(state);
		match self {
			Self::None | Self::Null => {}
			Self::Bool(value) => value.hash(state),
			Self::Number(value) => value.hash(state),
			Self::String(value) => value.hash(state),
			Self::Duration(value) => value.hash(state),
			Self::DateTime(value) => value.hash(state),
			Self::Ulid(value) => value.hash(state),
			Self::Uuid(value) => value.hash(state),
			Self::Array(value) => value.hash(state),
			Self::Set(value) => value.hash(state),
			Self::Object(value) => value.hash(state),
			Self::Geometry(value) => value.hash(state),
			Self::Vector(value) => value.hash(state),
			Self::Bytes(value) => value.hash(state),
			Self::Record(value) => value.hash(state),
			Self::Regex(value) => value.hash(state),
			Self::Range(value) => value.hash(state),
			Self::Graph(value) => value.hash(state),
			Self::Closure(value) => value.hash(state),
		}
	}
}

impl Display for Value {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::None => formatter.write_str("none"),
			Self::Null => formatter.write_str("null"),
			Self::Bool(value) => Display::fmt(value, formatter),
			Self::Number(value) => Display::fmt(value, formatter),
			Self::String(value) => write_json_string(formatter, value),
			Self::Duration(value) => Display::fmt(value, formatter),
			Self::DateTime(value) => Display::fmt(value, formatter),
			Self::Ulid(value) => Display::fmt(value, formatter),
			Self::Uuid(value) => Display::fmt(value, formatter),
			Self::Array(value) => write_sequence(formatter, "[", "]", value.iter()),
			Self::Set(value) => write_sequence(formatter, "set[", "]", value.iter()),
			Self::Object(value) => {
				formatter.write_str("{")?;
				for (index, (key, value)) in value.iter().enumerate() {
					if index > 0 {
						formatter.write_str(", ")?;
					}
					write_json_string(formatter, key)?;
					write!(formatter, ": {value}")?;
				}
				formatter.write_str("}")
			}
			Self::Geometry(value) => Display::fmt(value, formatter),
			Self::Vector(value) => match value {
				Vector::F32(values) => write_debug_sequence(formatter, "vector<f32>", values),
				Vector::F64(values) => write_debug_sequence(formatter, "vector<f64>", values),
				Vector::I16(values) => write_debug_sequence(formatter, "vector<i16>", values),
				Vector::I32(values) => write_debug_sequence(formatter, "vector<i32>", values),
				Vector::I64(values) => write_debug_sequence(formatter, "vector<i64>", values),
			},
			Self::Bytes(value) => {
				formatter.write_str("b\"")?;
				for byte in value {
					write!(formatter, "{byte:02x}")?;
				}
				formatter.write_str("\"")
			}
			Self::Record(value) => Display::fmt(value, formatter),
			Self::Regex(value) => Display::fmt(value, formatter),
			Self::Range(value) => Display::fmt(value, formatter),
			Self::Graph(value) => {
				write!(formatter, "graph({}@{})", value.identity(), value.revision(),)
			}
			Self::Closure(value) => write!(
				formatter,
				"<closure:{}#{}>",
				value.name_str().unwrap_or("anonymous"),
				value.id().get(),
			),
		}
	}
}

fn write_json_string(formatter: &mut Formatter<'_>, value: &str) -> fmt::Result {
	let value = serde_json::to_string(value).map_err(|_| fmt::Error)?;
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

impl From<bool> for Value {
	fn from(value: bool) -> Self {
		Self::Bool(value)
	}
}

macro_rules! impl_number_value {
	($($type:ty),* $(,)?) => {
		$(
			impl From<$type> for Value {
				fn from(value: $type) -> Self {
					Self::Number(Number::from(value))
				}
			}
		)*
	};
}

impl_number_value!(
	i8,
	i16,
	i32,
	i64,
	isize,
	u8,
	u16,
	u32,
	u64,
	usize,
	f32,
	f64,
	rust_decimal::Decimal,
);

impl From<Number> for Value {
	fn from(value: Number) -> Self {
		Self::Number(value)
	}
}

impl From<Text> for Value {
	fn from(value: Text) -> Self {
		Self::String(value)
	}
}

impl From<String> for Value {
	fn from(value: String) -> Self {
		Self::String(Text::from(value))
	}
}

impl From<&str> for Value {
	fn from(value: &str) -> Self {
		Self::String(Text::from(value))
	}
}

impl From<char> for Value {
	fn from(value: char) -> Self {
		Self::String(Text::from(value))
	}
}

macro_rules! impl_value_variant {
	($($type:ty => $variant:ident),* $(,)?) => {
		$(
			impl From<$type> for Value {
				fn from(value: $type) -> Self {
					Self::$variant(value)
				}
			}
		)*
	};
}

impl_value_variant!(
	Array => Array,
	Bytes => Bytes,
	DateTime => DateTime,
	Duration => Duration,
	Geometry => Geometry,
	Object => Object,
	Range => Range,
	Regex => Regex,
	Set => Set,
	Ulid => Ulid,
	Uuid => Uuid,
	Vector => Vector,
);

impl From<Vec<Value>> for Value {
	fn from(value: Vec<Value>) -> Self {
		Self::Array(Array::new(value))
	}
}

impl From<Closure> for Value {
	fn from(value: Closure) -> Self {
		Self::Closure(Box::new(value))
	}
}

impl From<RecordId> for Value {
	fn from(value: RecordId) -> Self {
		Self::Record(Box::new(value))
	}
}

impl From<Graph<Frozen>> for Value {
	fn from(value: Graph<Frozen>) -> Self {
		Self::Graph(Box::new(value))
	}
}

fn value_conversion_error(value: &Value, target: &'static str) -> FerrosGrynnError {
	FerrosGrynnError::conversion_error(value.kind().name(), target)
}

impl TryFrom<Value> for bool {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Bool(value) => Ok(value),
			value => Err(value_conversion_error(&value, "bool")),
		}
	}
}

impl TryFrom<Value> for Number {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Number(value) => Ok(value),
			value => Err(value_conversion_error(&value, "Number")),
		}
	}
}

macro_rules! try_number_from_value {
	($($type:ty),* $(,)?) => {
		$(
			impl TryFrom<Value> for $type {
				type Error = FerrosGrynnError;

				fn try_from(value: Value) -> Result<Self, Self::Error> {
					match value {
						Value::Number(value) => Self::try_from(value),
						value => Err(value_conversion_error(&value, stringify!($type))),
					}
				}
			}
		)*
	};
}

try_number_from_value!(
	i8,
	i16,
	i32,
	i64,
	i128,
	isize,
	u8,
	u16,
	u32,
	u64,
	u128,
	usize,
	f32,
	f64,
	rust_decimal::Decimal,
);

impl TryFrom<Value> for Text {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::String(value) => Ok(value),
			value => Err(value_conversion_error(&value, "Text")),
		}
	}
}

impl TryFrom<Value> for String {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		Text::try_from(value).map(Text::into_string)
	}
}

macro_rules! try_value_variant {
	($($type:ty => $variant:ident),* $(,)?) => {
		$(
			impl TryFrom<Value> for $type {
				type Error = FerrosGrynnError;

				fn try_from(value: Value) -> Result<Self, Self::Error> {
					match value {
						Value::$variant(value) => Ok(value),
						value => Err(value_conversion_error(&value, stringify!($type))),
					}
				}
			}
		)*
	};
}

try_value_variant!(
	Array => Array,
	Bytes => Bytes,
	DateTime => DateTime,
	Duration => Duration,
	Geometry => Geometry,
	Object => Object,
	Range => Range,
	Regex => Regex,
	Set => Set,
	Ulid => Ulid,
	Uuid => Uuid,
	Vector => Vector,
);

impl TryFrom<Value> for Closure {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Closure(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "Closure")),
		}
	}
}

impl TryFrom<Value> for RecordId {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Record(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "RecordId")),
		}
	}
}

impl TryFrom<Value> for Graph<Frozen> {
	type Error = FerrosGrynnError;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Graph(value) => Ok(*value),
			value => Err(value_conversion_error(&value, "Graph<Frozen>")),
		}
	}
}

#[cfg(test)]
mod tests {
	use std::collections::{BTreeSet, HashSet};

	use rust_decimal::Decimal;

	use super::{Number, Text, Type, Value, ValueKind};

	#[test]
	fn sentinel_semantics_are_two_valued() {
		assert_eq!(Value::None, Value::None);
		assert_eq!(Value::Null, Value::Null);
		assert_ne!(Value::None, Value::Null);
		assert_ne!(Value::Null, Value::from(1_i64));
		assert!(!Value::None.is_truthy());
		assert!(!Value::Null.is_truthy());
	}

	#[test]
	#[allow(clippy::mutable_key_type)]
	fn uses_explicit_stable_variant_order() {
		let values = BTreeSet::from([
			Value::from("string"),
			Value::Null,
			Value::from(true),
			Value::None,
			Value::from(1_i64),
		]);
		assert_eq!(values.into_iter().map(|value| value.kind()).collect::<Vec<_>>(), vec![
			ValueKind::None,
			ValueKind::Null,
			ValueKind::Bool,
			ValueKind::Int,
			ValueKind::String,
		],);
	}

	#[test]
	#[allow(clippy::mutable_key_type)]
	fn eq_ord_and_hash_agree_for_cross_number_values() {
		let values = HashSet::from([
			Value::from(Number::Int(1)),
			Value::from(Number::Float(1.0)),
			Value::from(Number::Decimal(Decimal::ONE)),
		]);
		assert_eq!(values.len(), 1);
	}

	#[test]
	fn reports_concrete_kind_and_static_type() {
		let value = Value::from(1.5_f64);
		assert_eq!(value.kind(), ValueKind::Float);
		assert_eq!(value.value_type(), Type::Float);
	}

	#[test]
	fn display_is_deterministic_and_escapes_strings() {
		let value = Value::from("a\0\nb");
		assert_eq!(value.to_string(), r#""a\u0000\nb""#);
	}

	#[test]
	fn runtime_only_values_are_explicitly_non_storable() {
		let closure = super::Closure::new(None, vec![], Type::None, |_| Ok(Value::None));
		let value = Value::from(vec![Value::from(closure)]);
		assert!(!value.is_storable());
		assert!(value.ensure_storable().is_err());
	}

	#[test]
	fn value_conversions_never_default_or_truncate() {
		assert_eq!(i64::try_from(Value::from(42_i64)).unwrap(), 42);
		assert!(i64::try_from(Value::from(42.5_f64)).is_err());
		assert!(bool::try_from(Value::Null).is_err());

		let value = Value::from("graph");
		assert_eq!(value.as_text(), Some(&Text::from("graph")));
		assert_eq!(Text::try_from(value).unwrap(), "graph");
	}

	#[test]
	fn value_respects_runtime_size_budget() {
		let size = std::mem::size_of::<Value>();
		assert!(
			size <= 64,
			"Value={size}, DateTime={}, RecordId={}, Geometry={}, Closure={}",
			std::mem::size_of::<super::DateTime>(),
			std::mem::size_of::<super::RecordId>(),
			std::mem::size_of::<super::Geometry>(),
			std::mem::size_of::<super::Closure>(),
		);
	}
}
