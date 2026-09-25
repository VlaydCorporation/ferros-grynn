//! Recursive schema-type codec reused by durable class fields.

use super::codec::{Decoder, invalid, read_string, write_string};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphKind},
	values::{Type, VectorKind},
};

const MAX_DEPTH: usize = 64;
const MAX_MEMBERS: usize = 4096;

#[derive(Clone, Copy)]
#[repr(u8)]
enum Tag {
	Any = 0x00,
	None = 0x01,
	Null = 0x02,
	Bool = 0x03,
	Number = 0x04,
	Int = 0x05,
	Float = 0x06,
	Decimal = 0x07,
	String = 0x08,
	Duration = 0x09,
	DateTime = 0x0a,
	Ulid = 0x0b,
	Uuid = 0x0c,
	Array = 0x0d,
	Set = 0x0e,
	Object = 0x0f,
	Geometry = 0x10,
	Vector = 0x11,
	Bytes = 0x12,
	Record = 0x13,
	Regex = 0x14,
	Range = 0x15,
	Atom = 0x16,
	Path = 0x17,
	Graph = 0x18,
	Option = 0x19,
	Union = 0x1a,
	RecordShape = 0x1b,
	Closure = 0x1c,
	GraphRef = 0x1d,
}

impl Tag {
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
			_ => return Err(invalid("database catalog contains an unknown schema type")),
		})
	}
}

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
		Type::Error | Type::Cursor => {
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

pub(super) fn decode(decoder: &mut Decoder<'_>, depth: usize) -> FerrosGrynnResult<Type> {
	if depth > MAX_DEPTH {
		return Err(invalid("schema type nesting exceeds 64 levels"));
	}
	Ok(match Tag::decode(decoder.read_u8()?)? {
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
		Tag::Range => Type::Range(Box::new(decode(decoder, depth + 1)?)),
		Tag::Atom => Type::Atom(decode_atom_kind(decoder.read_u8()?)?),
		Tag::Path => Type::Path,
		Tag::Graph => Type::Graph(decode_graph_kind(decoder.read_u8()?)?),
		Tag::Option => Type::Option(Box::new(decode(decoder, depth + 1)?)),
		Tag::Union => {
			let count = read_count(decoder)?;
			let mut values = Vec::with_capacity(count);
			for _ in 0..count {
				values.push(decode(decoder, depth + 1)?);
			}
			Type::Union(values)
		}
		Tag::RecordShape => {
			let count = read_count(decoder)?;
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

fn read_count(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<usize> {
	let count = usize::from(decoder.read_u16()?);
	if count > MAX_MEMBERS {
		return Err(invalid("schema type member count exceeds 4096"));
	}
	Ok(count)
}

const fn atom_kind(kind: AtomKind) -> u8 {
	match kind {
		AtomKind::Node => 0,
		AtomKind::Edge => 1,
	}
}

fn decode_atom_kind(value: u8) -> FerrosGrynnResult<AtomKind> {
	match value {
		0 => Ok(AtomKind::Node),
		1 => Ok(AtomKind::Edge),
		_ => Err(invalid("schema type contains an unknown AtomKind")),
	}
}

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

const fn vector_kind(kind: VectorKind) -> u8 {
	match kind {
		VectorKind::F64 => 0,
		VectorKind::F32 => 1,
		VectorKind::I64 => 2,
		VectorKind::I32 => 3,
		VectorKind::I16 => 4,
	}
}

fn decode_vector_kind(value: u8) -> FerrosGrynnResult<VectorKind> {
	match value {
		0 => Ok(VectorKind::F64),
		1 => Ok(VectorKind::F32),
		2 => Ok(VectorKind::I64),
		3 => Ok(VectorKind::I32),
		4 => Ok(VectorKind::I16),
		_ => Err(invalid("schema type contains an unknown VectorKind")),
	}
}
