use std::fmt::{self, Display, Formatter};

use super::VectorKind;

/// Logical value kind used to classify runtime values consistently.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum ValueKind {
	/// Logical kind identifying an absent value.
	None,
	/// Logical kind identifying an explicit null value.
	Null,
	/// Logical kind identifying a Boolean value.
	Bool,
	/// Logical kind identifying a signed integer.
	Int,
	/// Logical kind identifying a floating-point number.
	Float,
	/// Logical kind identifying a decimal number.
	Decimal,
	/// Logical kind identifying text.
	String,
	/// Logical kind identifying a duration.
	Duration,
	/// Logical kind identifying a time-zone-aware timestamp.
	DateTime,
	/// Logical kind identifying a ULID.
	Ulid,
	/// Logical kind identifying a UUID.
	Uuid,
	/// Logical kind identifying an ordered sequence of values.
	Array,
	/// Logical kind identifying an ordered set of distinct values.
	Set,
	/// Logical kind identifying a text-keyed value map.
	Object,
	/// Logical kind identifying a geometry value.
	Geometry,
	/// Logical kind identifying a numeric vector.
	Vector(
		/// Logical kind identifying a numeric vector.
		VectorKind,
	),
	/// Logical kind identifying binary bytes.
	Bytes,
	/// Logical kind identifying a stable graph reference.
	GraphRef,
	/// Logical kind identifying a class-qualified record identity.
	Record,
	/// Logical kind identifying a compiled regular expression.
	Regex,
	/// Logical kind identifying a runtime matcher.
	Matcher,
	/// Logical kind identifying a bounded or unbounded value interval.
	Range,
	/// Logical kind identifying a stable atom reference.
	Atom,
	/// Logical kind identifying a stable graph-qualified atom occurrence.
	AtomOccurrence,
	/// Logical kind identifying a snapshot-qualified path.
	Path,
	/// Logical kind identifying a graph-family subgraph value.
	Subgraph,
	/// Logical kind identifying a materialized graph family.
	Graph,
	/// Logical kind identifying a runtime callable.
	Closure,
	/// Logical kind identifying a runtime error value.
	Error,
	/// Logical kind identifying a runtime query cursor.
	Cursor,
}

impl ValueKind {
	/// Returns stable rank for `ValueKind`.
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
			Self::GraphRef => 15,
			Self::Record => 16,
			Self::Regex => 17,
			Self::Matcher => 18,
			Self::Range => 19,
			Self::Atom => 20,
			Self::AtomOccurrence => 21,
			Self::Path => 22,
			Self::Subgraph => 23,
			Self::Graph => 24,
			Self::Closure => 25,
			Self::Error => 26,
			Self::Cursor => 27,
		}
	}

	/// Returns name for `ValueKind`.
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
			Self::GraphRef => "graph_ref",
			Self::Record => "record",
			Self::Regex => "regex",
			Self::Matcher => "matcher",
			Self::Range => "range",
			Self::Atom => "atom",
			Self::AtomOccurrence => "atom_occurrence",
			Self::Path => "path",
			Self::Subgraph => "subgraph",
			Self::Graph => "graph",
			Self::Closure => "closure",
			Self::Error => "error",
			Self::Cursor => "cursor",
		}
	}

	/// Reports whether the value is number for `ValueKind`.
	pub const fn is_number(self) -> bool {
		matches!(self, Self::Int | Self::Float | Self::Decimal)
	}

	/// Reports whether the value is compatible with for `ValueKind`.
	pub fn is_compatible_with(self, other: Self) -> bool {
		(self.is_number() && other.is_number()) || self == other
	}
}

impl Display for ValueKind {
	/// Formats `ValueKind` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(self.name())
	}
}
