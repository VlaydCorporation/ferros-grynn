//! Recursive schema-type codec reused by durable class fields.

use super::codec::{Decoder, invalid, read_string, write_string};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphKind},
	values::{Type, VectorKind},
};

/// Configured max depth value.
const MAX_DEPTH: usize = 64;
/// Configured max members value.
const MAX_MEMBERS: usize = 4096;

#[derive(Clone, Copy)]
#[repr(u8)]
/// Tag alternatives.
enum Tag {
	/// Type constraint accepting an unconstrained value.
	Any = 0x00,
	/// Type constraint accepting an absent value.
	None = 0x01,
	/// Type constraint accepting an explicit null value.
	Null = 0x02,
	/// Type constraint accepting a Boolean value.
	Bool = 0x03,
	/// Type constraint accepting a numeric value.
	Number = 0x04,
	/// Type constraint accepting a signed integer.
	Int = 0x05,
	/// Type constraint accepting a floating-point number.
	Float = 0x06,
	/// Type constraint accepting a decimal number.
	Decimal = 0x07,
	/// Type constraint accepting text.
	String = 0x08,
	/// Type constraint accepting a duration.
	Duration = 0x09,
	/// Type constraint accepting a time-zone-aware timestamp.
	DateTime = 0x0a,
	/// Type constraint accepting a ULID.
	Ulid = 0x0b,
	/// Type constraint accepting a UUID.
	Uuid = 0x0c,
	/// Type constraint accepting an ordered sequence of values.
	Array = 0x0d,
	/// Type constraint accepting an ordered set of distinct values.
	Set = 0x0e,
	/// Type constraint accepting a text-keyed value map.
	Object = 0x0f,
	/// Type constraint accepting a geometry value.
	Geometry = 0x10,
	/// Type constraint accepting a numeric vector.
	Vector = 0x11,
	/// Type constraint accepting binary bytes.
	Bytes = 0x12,
	/// Type constraint accepting a class-qualified record identity.
	Record = 0x13,
	/// Type constraint accepting a compiled regular expression.
	Regex = 0x14,
	/// Type constraint accepting a bounded or unbounded value interval.
	Range = 0x15,
	/// Type constraint accepting a stable atom reference.
	Atom = 0x16,
	/// Type constraint accepting a snapshot-qualified path.
	Path = 0x17,
	/// Type constraint accepting a materialized graph family.
	Graph = 0x18,
	/// Type constraint accepting an optional value.
	Option = 0x19,
	/// Type constraint accepting a union of accepted value types.
	Union = 0x1a,
	/// Type constraint accepting a structured record constraint.
	RecordShape = 0x1b,
	/// Type constraint accepting a runtime callable.
	Closure = 0x1c,
	/// Type constraint accepting a stable graph reference.
	GraphRef = 0x1d,
	/// Type constraint accepting a runtime matcher.
	Matcher = 0x1e,
}

impl Tag {
	/// Decodes tag.
	fn decode(value: u8) -> FerrosGrynnResult<Self> {
		Ok(match value {
			0x00 => Self::Any,
			0x01 => Self::None,
			0x02 => Self::Null,
			0x03 => Self::Bool,
			0x04 => Self::Number,
			0x05 => Self::Int,
			0x06 => Self::Float,
			0x07 => Self::Decimal,
			0x08 => Self::String,
			0x09 => Self::Duration,
			0x0a => Self::DateTime,
			0x0b => Self::Ulid,
			0x0c => Self::Uuid,
			0x0d => Self::Array,
			0x0e => Self::Set,
			0x0f => Self::Object,
			0x10 => Self::Geometry,
			0x11 => Self::Vector,
			0x12 => Self::Bytes,
			0x13 => Self::Record,
			0x14 => Self::Regex,
			0x15 => Self::Range,
			0x16 => Self::Atom,
			0x17 => Self::Path,
			0x18 => Self::Graph,
			0x19 => Self::Option,
			0x1a => Self::Union,
			0x1b => Self::RecordShape,
			0x1c => Self::Closure,
			0x1d => Self::GraphRef,
			0x1e => Self::Matcher,
			_ => return Err(invalid("database catalog contains an unknown schema type")),
		})
	}
}

/// Encodes the current state.
pub(super) fn encode(output: &mut Vec<u8>, value: &Type, depth: usize) -> FerrosGrynnResult<()> {
	if depth > MAX_DEPTH {
		return Err(invalid("schema type nesting exceeds 64 levels"));
	}
	let tag = match value {
		Type::Any => Tag::Any,
		Type::None => Tag::None,
		Type::Null => Tag::Null,
		Type::Bool => Tag::Bool,
		Type::Number => Tag::Number,
		Type::Int => Tag::Int,
		Type::Float => Tag::Float,
		Type::Decimal => Tag::Decimal,
		Type::String => Tag::String,
		Type::Duration => Tag::Duration,
		Type::DateTime => Tag::DateTime,
		Type::Ulid => Tag::Ulid,
		Type::Uuid => Tag::Uuid,
		Type::Array(_) => Tag::Array,
		Type::Set(_) => Tag::Set,
		Type::Object => Tag::Object,
		Type::Geometry => Tag::Geometry,
		Type::Vector(_) => Tag::Vector,
		Type::Bytes => Tag::Bytes,
		Type::GraphRef(_) => Tag::GraphRef,
		Type::Record => Tag::Record,
		Type::Regex => Tag::Regex,
		Type::Matcher => Tag::Matcher,
		Type::Range(_) => Tag::Range,
		Type::Atom(_) => Tag::Atom,
		Type::Path => Tag::Path,
		Type::Graph(_) => Tag::Graph,
		Type::Option(_) => Tag::Option,
		Type::Union(_) => Tag::Union,
		Type::RecordShape(_) => Tag::RecordShape,
		Type::Closure {
			..
		} => Tag::Closure,
		Type::AtomOccurrence(_) | Type::Error | Type::Cursor => {
			return Err(invalid("runtime-only values cannot appear in a durable schema"));
		}
	};
	output.push(tag as u8);
	match value {
		Type::Array(inner) | Type::Set(inner) | Type::Range(inner) | Type::Option(inner) => {
			encode(output, inner, depth + 1)?;
		}
		Type::Vector(kind) => output.push(vector_kind(*kind)),
		Type::GraphRef(kind) | Type::Graph(kind) => output.push(graph_kind(*kind)),
		Type::Atom(kind) => output.push(atom_kind(*kind)),
		Type::AtomOccurrence(_) => {
			return Err(invalid("runtime-only values cannot appear in a durable schema"));
		}
		Type::Union(values) => {
			write_count(output, values.len())?;
			for value in values {
				encode(output, value, depth + 1)?;
			}
		}
		Type::RecordShape(fields) => {
			write_count(output, fields.len())?;
			for (name, value) in fields {
				write_string(output, name.as_str(), "record-shape field")?;
				encode(output, value, depth + 1)?;
			}
		}
		Type::Closure {
			parameters,
			result,
		} => {
			write_count(output, parameters.len())?;
			for parameter in parameters {
				encode(output, parameter, depth + 1)?;
			}
			encode(output, result, depth + 1)?;
		}
		_ => {}
	}
	Ok(())
}

/// Decodes the current state.
pub(super) fn decode(decoder: &mut Decoder<'_>, depth: usize) -> FerrosGrynnResult<Type> {
	if depth > MAX_DEPTH {
		return Err(invalid("schema type nesting exceeds 64 levels"));
	}
	let tag = Tag::decode(decoder.read_u8()?)?;
	if matches!(tag, Tag::Array | Tag::Set | Tag::Range | Tag::Option | Tag::Closure) {
		decoder.admit_heap(std::mem::size_of::<Type>())?;
	}
	Ok(match tag {
		Tag::Any => Type::Any,
		Tag::None => Type::None,
		Tag::Null => Type::Null,
		Tag::Bool => Type::Bool,
		Tag::Number => Type::Number,
		Tag::Int => Type::Int,
		Tag::Float => Type::Float,
		Tag::Decimal => Type::Decimal,
		Tag::String => Type::String,
		Tag::Duration => Type::Duration,
		Tag::DateTime => Type::DateTime,
		Tag::Ulid => Type::Ulid,
		Tag::Uuid => Type::Uuid,
		Tag::Array => Type::Array(Box::new(decode(decoder, depth + 1)?)),
		Tag::Set => Type::Set(Box::new(decode(decoder, depth + 1)?)),
		Tag::Object => Type::Object,
		Tag::Geometry => Type::Geometry,
		Tag::Vector => Type::Vector(decode_vector_kind(decoder.read_u8()?)?),
		Tag::Bytes => Type::Bytes,
		Tag::GraphRef => Type::GraphRef(decode_graph_kind(decoder.read_u8()?)?),
		Tag::Record => Type::Record,
		Tag::Regex => Type::Regex,
		Tag::Matcher => Type::Matcher,
		Tag::Range => Type::Range(Box::new(decode(decoder, depth + 1)?)),
		Tag::Atom => Type::Atom(decode_atom_kind(decoder.read_u8()?)?),
		Tag::Path => Type::Path,
		Tag::Graph => Type::Graph(decode_graph_kind(decoder.read_u8()?)?),
		Tag::Option => Type::Option(Box::new(decode(decoder, depth + 1)?)),
		Tag::Union => {
			let count = read_count(decoder)?;
			check_member_bytes(decoder, count, 1)?;
			decoder.admit_heap(
				count
					.checked_mul(std::mem::size_of::<Type>())
					.ok_or_else(|| invalid("schema type allocation overflows"))?,
			)?;
			let mut values = Vec::with_capacity(count);
			for _ in 0..count {
				values.push(decode(decoder, depth + 1)?);
			}
			Type::Union(values)
		}
		Tag::RecordShape => {
			let count = read_count(decoder)?;
			check_member_bytes(decoder, count, 4)?;
			decoder.admit_heap(
				count
					.checked_mul(std::mem::size_of::<(crate::values::Text, Type)>())
					.ok_or_else(|| invalid("schema type allocation overflows"))?,
			)?;
			let mut fields = Vec::with_capacity(count);
			for _ in 0..count {
				fields.push((
					read_string(decoder, "record-shape field")?.into(),
					decode(decoder, depth + 1)?,
				));
			}
			Type::RecordShape(fields)
		}
		Tag::Closure => {
			let count = read_count(decoder)?;
			check_member_bytes(decoder, count.saturating_add(1), 1)?;
			decoder.admit_heap(
				count
					.checked_mul(std::mem::size_of::<Type>())
					.ok_or_else(|| invalid("schema type allocation overflows"))?,
			)?;
			let mut parameters = Vec::with_capacity(count);
			for _ in 0..count {
				parameters.push(decode(decoder, depth + 1)?);
			}
			Type::Closure {
				parameters,
				result: Box::new(decode(decoder, depth + 1)?),
			}
		}
	})
}

/// Writes count.
fn write_count(output: &mut Vec<u8>, count: usize) -> FerrosGrynnResult<()> {
	if count > MAX_MEMBERS {
		return Err(invalid("schema type member count exceeds 4096"));
	}
	output.extend_from_slice(
		&u16::try_from(count)
			.map_err(|_error| invalid("schema type member count exceeds u16"))?
			.to_le_bytes(),
	);
	Ok(())
}

/// Reads count.
fn read_count(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<usize> {
	let count = usize::from(decoder.read_u16()?);
	if count > MAX_MEMBERS {
		return Err(invalid("schema type member count exceeds 4096"));
	}
	Ok(count)
}

/// Persisted counts must have enough encoded members before reserving slots.
fn check_member_bytes(
	decoder: &Decoder<'_>,
	count: usize,
	minimum: usize,
) -> FerrosGrynnResult<()> {
	if count > decoder.remaining_bytes() / minimum {
		return Err(invalid("schema type member count exceeds remaining bytes"));
	}
	Ok(())
}

/// Returns the atom kind result.
const fn atom_kind(kind: AtomKind) -> u8 {
	match kind {
		AtomKind::Node => 0,
		AtomKind::Edge => 1,
	}
}

/// Decodes atom kind.
fn decode_atom_kind(value: u8) -> FerrosGrynnResult<AtomKind> {
	match value {
		0 => Ok(AtomKind::Node),
		1 => Ok(AtomKind::Edge),
		_ => Err(invalid("schema type contains an unknown AtomKind")),
	}
}

/// Returns the graph kind result.
pub(super) const fn graph_kind(kind: GraphKind) -> u8 {
	match kind {
		GraphKind::Graph => 0,
		GraphKind::Tree => 1,
		GraphKind::RootedTree => 2,
		GraphKind::Forest => 3,
		GraphKind::Dag => 4,
		GraphKind::Hypergraph => 5,
		GraphKind::Metagraph => 6,
		GraphKind::Subgraph => 7,
	}
}

/// Decodes graph kind.
pub(super) fn decode_graph_kind(value: u8) -> FerrosGrynnResult<GraphKind> {
	match value {
		0 => Ok(GraphKind::Graph),
		1 => Ok(GraphKind::Tree),
		2 => Ok(GraphKind::RootedTree),
		3 => Ok(GraphKind::Forest),
		4 => Ok(GraphKind::Dag),
		5 => Ok(GraphKind::Hypergraph),
		6 => Ok(GraphKind::Metagraph),
		7 => Ok(GraphKind::Subgraph),
		_ => Err(invalid("database catalog contains an unknown GraphKind")),
	}
}

/// Returns the vector kind result.
const fn vector_kind(kind: VectorKind) -> u8 {
	match kind {
		VectorKind::F64 => 0,
		VectorKind::F32 => 1,
		VectorKind::I64 => 2,
		VectorKind::I32 => 3,
		VectorKind::I16 => 4,
		VectorKind::F16 => 5,
		VectorKind::I8 => 6,
		VectorKind::U8 => 7,
	}
}

/// Decodes vector kind.
fn decode_vector_kind(value: u8) -> FerrosGrynnResult<VectorKind> {
	match value {
		0 => Ok(VectorKind::F64),
		1 => Ok(VectorKind::F32),
		2 => Ok(VectorKind::I64),
		3 => Ok(VectorKind::I32),
		4 => Ok(VectorKind::I16),
		5 => Ok(VectorKind::F16),
		6 => Ok(VectorKind::I8),
		7 => Ok(VectorKind::U8),
		_ => Err(invalid("schema type contains an unknown VectorKind")),
	}
}
