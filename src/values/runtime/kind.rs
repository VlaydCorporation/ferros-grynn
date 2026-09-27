use std::fmt::{self, Display, Formatter};

use super::VectorKind;

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
	GraphRef,
	Record,
	Regex,
	Range,
	Atom,
	AtomOccurrence,
	Path,
	Subgraph,
	Graph,
	Closure,
	Error,
	Cursor,
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
			Self::GraphRef => 15,
			Self::Record => 16,
			Self::Regex => 17,
			Self::Range => 18,
			Self::Atom => 19,
			Self::AtomOccurrence => 20,
			Self::Path => 21,
			Self::Subgraph => 22,
			Self::Graph => 23,
			Self::Closure => 24,
			Self::Error => 25,
			Self::Cursor => 26,
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
			Self::GraphRef => "graph_ref",
			Self::Record => "record",
			Self::Regex => "regex",
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

	pub const fn is_number(self) -> bool {
		matches!(self, Self::Int | Self::Float | Self::Decimal)
	}

	pub fn is_compatible_with(self, other: Self) -> bool {
		(self.is_number() && other.is_number()) || self == other
	}
}

impl Display for ValueKind {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(self.name())
	}
}
