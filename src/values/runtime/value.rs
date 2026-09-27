use std::{
	cmp::Ordering,
	hash::{Hash, Hasher},
	mem::{size_of, size_of_val},
	ops::Bound,
};

use super::{
	Array,
	AtomOccurrenceRef,
	AtomRef,
	Bytes,
	Closure,
	DateTime,
	Duration,
	ErrorValue,
	Frozen,
	Geometry,
	Graph,
	GraphKind,
	GraphRef,
	MaterializedGraph,
	Number,
	Object,
	Path,
	Position,
	QueryCursor,
	Range,
	RecordId,
	RecordIdPart,
	Regex,
	Set,
	Text,
	Type,
	Ulid,
	Uuid,
	ValueKind,
	Vector,
};
use crate::gql_status::FerrosGrynnError;

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
	GraphRef(Box<GraphRef>),
	Record(Box<RecordId>),
	Regex(Regex),
	Range(Range),
	Atom(Box<AtomRef>),
	AtomOccurrence(Box<AtomOccurrenceRef>),
	Path(Box<Path>),
	Graph(Box<MaterializedGraph>),
	Closure(Box<Closure>),
	Error(Box<ErrorValue>),
	Cursor(QueryCursor),
}

impl Value {
	pub fn kind(&self) -> ValueKind {
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
			Self::GraphRef(_) => ValueKind::GraphRef,
			Self::Record(_) => ValueKind::Record,
			Self::Regex(_) => ValueKind::Regex,
			Self::Range(_) => ValueKind::Range,
			Self::Atom(_) => ValueKind::Atom,
			Self::AtomOccurrence(_) => ValueKind::AtomOccurrence,
			Self::Path(_) => ValueKind::Path,
			Self::Graph(value) if value.kind() == GraphKind::Subgraph => ValueKind::Subgraph,
			Self::Graph(_) => ValueKind::Graph,
			Self::Closure(_) => ValueKind::Closure,
			Self::Error(_) => ValueKind::Error,
			Self::Cursor(_) => ValueKind::Cursor,
		}
	}

	/// Estimates heap bytes owned by this value without allocating or cloning
	/// it.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		match self {
			Self::None
			| Self::Null
			| Self::Bool(_)
			| Self::Number(_)
			| Self::Duration(_)
			| Self::DateTime(_)
			| Self::Ulid(_)
			| Self::Uuid(_)
			| Self::Cursor(_) => 0,
			Self::String(value) => value.len(),
			Self::Array(values) => estimated_values_heap_bytes(values.iter(), values.len()),
			Self::Set(values) => estimated_values_heap_bytes(values.iter(), values.len()),
			Self::Object(values) => values
				.iter()
				.map(|(key, value)| {
					size_of::<(Text, Value)>()
						.saturating_add(key.len())
						.saturating_add(value.estimated_heap_bytes())
				})
				.fold(0_usize, usize::saturating_add),
			Self::Geometry(value) => estimated_geometry_heap_bytes(value),
			Self::Vector(value) => estimated_vector_heap_bytes(value),
			Self::Bytes(value) => value.len(),
			Self::GraphRef(value) => size_of::<GraphRef>().saturating_add(value.name().len()),
			Self::Record(value) => {
				size_of::<RecordId>().saturating_add(estimated_record_id_heap_bytes(value))
			}
			Self::Regex(value) => value.as_str().len(),
			Self::Range(value) => estimated_bound_heap_bytes(value.start())
				.saturating_add(estimated_bound_heap_bytes(value.end())),
			Self::Atom(value) => {
				size_of::<AtomRef>().saturating_add(estimated_record_id_heap_bytes(value.id()))
			}
			Self::AtomOccurrence(value) => size_of::<AtomOccurrenceRef>()
				.saturating_add(estimated_record_id_heap_bytes(value.atom().id())),
			Self::Path(value) => size_of_val(value.as_ref()),
			Self::Graph(value) => value.estimated_heap_bytes(),
			Self::Closure(value) => size_of_val(value.as_ref())
				.saturating_add(value.name().map_or(0, Text::len))
				.saturating_add(value.parameters().len().saturating_mul(size_of::<Type>())),
			Self::Error(value) => size_of_val(value.as_ref()),
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
			| Self::GraphRef(_)
			| Self::Record(_)
			| Self::Regex(_)
			| Self::Range(_)
			| Self::Atom(_)
			| Self::AtomOccurrence(_)
			| Self::Path(_)
			| Self::Graph(_)
			| Self::Closure(_) => true,
			Self::Error(_) | Self::Cursor(_) => true,
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

	pub const fn as_graph_ref(&self) -> Option<&GraphRef> {
		match self {
			Self::GraphRef(value) => Some(value),
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

	pub fn as_atom(&self) -> Option<&AtomRef> {
		match self {
			Self::Atom(value) => Some(value),
			_ => None,
		}
	}

	pub fn as_atom_occurrence(&self) -> Option<&AtomOccurrenceRef> {
		match self {
			Self::AtomOccurrence(value) => Some(value),
			_ => None,
		}
	}

	pub fn as_path(&self) -> Option<&Path> {
		match self {
			Self::Path(value) => Some(value),
			_ => None,
		}
	}

	pub fn as_materialized_graph(&self) -> Option<&MaterializedGraph> {
		match self {
			Self::Graph(value) => Some(value),
			_ => None,
		}
	}

	pub fn as_graph(&self) -> Option<&Graph<Frozen>> {
		self.as_materialized_graph().and_then(MaterializedGraph::as_general)
	}

	pub const fn as_closure(&self) -> Option<&Closure> {
		match self {
			Self::Closure(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_error(&self) -> Option<&ErrorValue> {
		match self {
			Self::Error(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_cursor(&self) -> Option<QueryCursor> {
		match self {
			Self::Cursor(value) => Some(*value),
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
			Self::Regex(_)
			| Self::AtomOccurrence(_)
			| Self::Path(_)
			| Self::Graph(_)
			| Self::Closure(_)
			| Self::Error(_)
			| Self::Cursor(_) => false,
			_ => true,
		}
	}

	pub fn ensure_storable(&self) -> Result<(), FerrosGrynnError> {
		self.is_storable()
			.then_some(())
			.ok_or_else(|| FerrosGrynnError::conversion_error(self.kind().name(), "StorageValue"))
	}
}

fn estimated_values_heap_bytes<'a>(
	values: impl Iterator<Item = &'a Value>,
	length: usize,
) -> usize {
	length.saturating_mul(size_of::<Value>()).saturating_add(
		values.map(Value::estimated_heap_bytes).fold(0_usize, usize::saturating_add),
	)
}

pub(super) fn estimated_record_id_heap_bytes(record: &RecordId) -> usize {
	record.class().len().saturating_add(estimated_record_part_heap_bytes(record.id()))
}

fn estimated_record_part_heap_bytes(part: &RecordIdPart) -> usize {
	match part {
		RecordIdPart::Int(_) | RecordIdPart::Uuid(_) | RecordIdPart::Ulid(_) => 0,
		RecordIdPart::String(value) => value.len(),
		RecordIdPart::Array(values) => estimated_values_heap_bytes(values.iter(), values.len()),
		RecordIdPart::Object(values) => values
			.iter()
			.map(|(key, value)| {
				size_of::<(Text, Value)>()
					.saturating_add(key.len())
					.saturating_add(value.estimated_heap_bytes())
			})
			.fold(0_usize, usize::saturating_add),
	}
}

fn estimated_bound_heap_bytes(bound: Bound<&Value>) -> usize {
	match bound {
		Bound::Included(value) | Bound::Excluded(value) => {
			size_of::<Value>().saturating_add(value.estimated_heap_bytes())
		}
		Bound::Unbounded => 0,
	}
}

fn estimated_position_heap_bytes(position: &Position) -> usize {
	position.as_slice().len().saturating_mul(size_of::<f64>())
}

fn estimated_positions_heap_bytes(positions: &[Position]) -> usize {
	positions.len().saturating_mul(size_of::<Position>()).saturating_add(
		positions.iter().map(estimated_position_heap_bytes).fold(0_usize, usize::saturating_add),
	)
}

fn estimated_geometry_heap_bytes(geometry: &Geometry) -> usize {
	match geometry {
		Geometry::Point(position) => estimated_position_heap_bytes(position),
		Geometry::MultiPoint(positions) | Geometry::LineString(positions) => {
			estimated_positions_heap_bytes(positions)
		}
		Geometry::MultiLineString(lines) | Geometry::Polygon(lines) => lines
			.iter()
			.map(|line| {
				size_of::<Vec<Position>>().saturating_add(estimated_positions_heap_bytes(line))
			})
			.fold(0_usize, usize::saturating_add),
		Geometry::MultiPolygon(polygons) => polygons
			.iter()
			.flat_map(|polygon| polygon.iter())
			.map(|ring| {
				size_of::<Vec<Position>>().saturating_add(estimated_positions_heap_bytes(ring))
			})
			.fold(0_usize, usize::saturating_add),
		Geometry::Collection(values) => values
			.iter()
			.map(|value| size_of::<Geometry>().saturating_add(estimated_geometry_heap_bytes(value)))
			.fold(0_usize, usize::saturating_add),
	}
}

fn estimated_vector_heap_bytes(vector: &Vector) -> usize {
	match vector {
		Vector::F32(values) => values.len().saturating_mul(size_of::<f32>()),
		Vector::F64(values) => values.len().saturating_mul(size_of::<f64>()),
		Vector::I16(values) => values.len().saturating_mul(size_of::<i16>()),
		Vector::I32(values) => values.len().saturating_mul(size_of::<i32>()),
		Vector::I64(values) => values.len().saturating_mul(size_of::<i64>()),
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
				(Self::GraphRef(left), Self::GraphRef(right)) => left.cmp(right),
				(Self::Record(left), Self::Record(right)) => left.cmp(right),
				(Self::Regex(left), Self::Regex(right)) => left.cmp(right),
				(Self::Range(left), Self::Range(right)) => left.cmp(right),
				(Self::Atom(left), Self::Atom(right)) => left.cmp(right),
				(Self::AtomOccurrence(left), Self::AtomOccurrence(right)) => left.cmp(right),
				(Self::Path(left), Self::Path(right)) => left.cmp(right),
				(Self::Graph(left), Self::Graph(right)) => left.cmp(right),
				(Self::Closure(left), Self::Closure(right)) => left.cmp(right),
				(Self::Error(left), Self::Error(right)) => left.cmp(right),
				(Self::Cursor(left), Self::Cursor(right)) => left.cmp(right),
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
			Self::GraphRef(value) => value.hash(state),
			Self::Record(value) => value.hash(state),
			Self::Regex(value) => value.hash(state),
			Self::Range(value) => value.hash(state),
			Self::Atom(value) => value.hash(state),
			Self::AtomOccurrence(value) => value.hash(state),
			Self::Path(value) => value.hash(state),
			Self::Graph(value) => value.hash(state),
			Self::Closure(value) => value.hash(state),
			Self::Error(value) => value.hash(state),
			Self::Cursor(value) => value.hash(state),
		}
	}
}
