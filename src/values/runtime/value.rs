use std::{
	cmp::Ordering,
	collections::BTreeSet,
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
	Matcher,
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

/// Owned runtime value used by the Rust SDK and query execution.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub enum Value {
	/// Runtime representation of an absent value.
	#[default]
	None,
	/// Runtime representation of an explicit null value.
	Null,
	/// Runtime representation of a Boolean value.
	Bool(
		/// Runtime representation of a Boolean value.
		bool,
	),
	/// Runtime representation of a numeric value.
	Number(
		/// Runtime representation of a numeric value.
		Number,
	),
	/// Runtime representation of text.
	String(
		/// Runtime representation of text.
		Text,
	),
	/// Runtime representation of a duration.
	Duration(
		/// Runtime representation of a duration.
		Duration,
	),
	/// Runtime representation of a time-zone-aware timestamp.
	DateTime(
		/// Runtime representation of a time-zone-aware timestamp.
		DateTime,
	),
	/// Runtime representation of a ULID.
	Ulid(
		/// Runtime representation of a ULID.
		Ulid,
	),
	/// Runtime representation of a UUID.
	Uuid(
		/// Runtime representation of a UUID.
		Uuid,
	),
	/// Runtime representation of an ordered sequence of values.
	Array(
		/// Runtime representation of an ordered sequence of values.
		Array,
	),
	/// Runtime representation of an ordered set of distinct values.
	Set(
		/// Runtime representation of an ordered set of distinct values.
		Set,
	),
	/// Runtime representation of a text-keyed value map.
	Object(
		/// Runtime representation of a text-keyed value map.
		Object,
	),
	/// Runtime representation of a geometry value.
	Geometry(
		/// Runtime representation of a geometry value.
		Geometry,
	),
	/// Runtime representation of a numeric vector.
	Vector(
		/// Runtime representation of a numeric vector.
		Vector,
	),
	/// Runtime representation of binary bytes.
	Bytes(
		/// Runtime representation of binary bytes.
		Bytes,
	),
	/// Runtime representation of a stable graph reference.
	GraphRef(
		/// Runtime representation of a stable graph reference.
		Box<GraphRef>,
	),
	/// Runtime representation of a class-qualified record identity.
	Record(
		/// Runtime representation of a class-qualified record identity.
		Box<RecordId>,
	),
	/// Runtime representation of a compiled regular expression.
	Regex(
		/// Runtime representation of a compiled regular expression.
		Regex,
	),
	/// Runtime representation of a runtime matcher.
	Matcher(
		/// Runtime representation of a runtime matcher.
		Matcher,
	),
	/// Runtime representation of a bounded or unbounded value interval.
	Range(
		/// Runtime representation of a bounded or unbounded value interval.
		Range,
	),
	/// Runtime representation of a stable atom reference.
	Atom(
		/// Runtime representation of a stable atom reference.
		Box<AtomRef>,
	),
	/// Runtime representation of a stable graph-qualified atom occurrence.
	AtomOccurrence(
		/// Runtime representation of a stable graph-qualified atom occurrence.
		Box<AtomOccurrenceRef>,
	),
	/// Runtime representation of a snapshot-qualified path.
	Path(
		/// Runtime representation of a snapshot-qualified path.
		Box<Path>,
	),
	/// Runtime representation of a materialized graph family.
	Graph(
		/// Runtime representation of a materialized graph family.
		Box<MaterializedGraph>,
	),
	/// Runtime representation of a runtime callable.
	Closure(
		/// Runtime representation of a runtime callable.
		Box<Closure>,
	),
	/// Runtime representation of a runtime error value.
	Error(
		/// Runtime representation of a runtime error value.
		Box<ErrorValue>,
	),
	/// Runtime representation of a runtime query cursor.
	Cursor(
		/// Runtime representation of a runtime query cursor.
		QueryCursor,
	),
}

impl Value {
	/// Collects every session-owned closure reachable through this runtime
	/// value.
	pub(crate) fn collect_session_closure_ids(&self, ids: &mut BTreeSet<super::RuntimeId>) {
		match self {
			Self::Closure(closure) if closure.session_owner().is_some() => {
				ids.insert(closure.id());
			}
			Self::Array(values) => {
				for value in values {
					value.collect_session_closure_ids(ids);
				}
			}
			Self::Set(values) => {
				for value in values {
					value.collect_session_closure_ids(ids);
				}
			}
			Self::Object(values) => {
				for value in values.values() {
					value.collect_session_closure_ids(ids);
				}
			}
			Self::Range(range) => {
				if let Some(value) = bound_value(range.start()) {
					value.collect_session_closure_ids(ids);
				}
				if let Some(value) = bound_value(range.end()) {
					value.collect_session_closure_ids(ids);
				}
			}
			_ => {}
		}
	}

	/// Returns the first session-owned closure retained by this runtime value.
	///
	/// Temporary GrynnQL sessions use this recursive check at their public
	/// result boundary because such a closure becomes unusable as soon as the
	/// session registry is dropped.
	pub(crate) fn session_closure_id(&self) -> Option<super::RuntimeId> {
		match self {
			Self::Closure(closure) => closure.session_owner().map(|_| closure.id()),
			Self::Array(values) => values.iter().find_map(Self::session_closure_id),
			Self::Set(values) => values.iter().find_map(Self::session_closure_id),
			Self::Object(values) => values.values().find_map(Self::session_closure_id),
			Self::Range(range) => bound_value(range.start())
				.and_then(Self::session_closure_id)
				.or_else(|| bound_value(range.end()).and_then(Self::session_closure_id)),
			_ => None,
		}
	}

	/// Returns kind for `Value`.
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
			Self::Matcher(_) => ValueKind::Matcher,
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
			Self::String(value) => value.estimated_heap_bytes(),
			Self::Array(values) => estimated_values_heap_bytes(values.iter(), values.capacity()),
			Self::Set(values) => values
				.iter()
				.map(Self::estimated_heap_bytes)
				.fold(estimated_tree_heap_bytes::<Value>(values.len()), usize::saturating_add),
			Self::Object(values) => values
				.iter()
				.map(|(key, value)| {
					key.estimated_heap_bytes().saturating_add(value.estimated_heap_bytes())
				})
				.fold(
					estimated_tree_heap_bytes::<(Text, Value)>(values.len()),
					usize::saturating_add,
				),
			Self::Geometry(value) => estimated_geometry_heap_bytes(value),
			Self::Vector(value) => estimated_vector_heap_bytes(value),
			Self::Bytes(value) => value.estimated_heap_bytes(),
			Self::GraphRef(value) => {
				size_of::<GraphRef>().saturating_add(value.name().estimated_heap_bytes())
			}
			Self::Record(value) => {
				size_of::<RecordId>().saturating_add(estimated_record_id_heap_bytes(value))
			}
			Self::Regex(value) => value.as_str().len(),
			Self::Matcher(value) => value.estimated_heap_bytes(),
			Self::Range(value) => estimated_bound_heap_bytes(value.start())
				.saturating_add(estimated_bound_heap_bytes(value.end())),
			Self::Atom(value) => {
				size_of::<AtomRef>().saturating_add(estimated_record_id_heap_bytes(value.id()))
			}
			Self::AtomOccurrence(value) => size_of::<AtomOccurrenceRef>()
				.saturating_add(estimated_record_id_heap_bytes(value.atom().id()))
				.saturating_add(size_of::<GraphRef>())
				.saturating_add(value.graph().name().estimated_heap_bytes()),
			Self::Path(value) => {
				size_of_val(value.as_ref()).saturating_add(value.estimated_heap_bytes())
			}
			Self::Graph(value) => value.estimated_heap_bytes(),
			Self::Closure(value) => size_of_val(value.as_ref())
				.saturating_add(value.name().map_or(0, Text::len))
				.saturating_add(value.parameters().len().saturating_mul(size_of::<Type>())),
			Self::Error(value) => size_of_val(value.as_ref()),
		}
	}

	/// Returns value type for `Value`.
	pub fn value_type(&self) -> Type {
		Type::of(self)
	}

	/// Reports whether the value is none for `Value`.
	pub const fn is_none(&self) -> bool {
		matches!(self, Self::None)
	}

	/// Reports whether the value is null for `Value`.
	pub const fn is_null(&self) -> bool {
		matches!(self, Self::Null)
	}

	/// Reports whether the value is truthy for `Value`.
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
			| Self::Matcher(_)
			| Self::Range(_)
			| Self::Atom(_)
			| Self::AtomOccurrence(_)
			| Self::Path(_)
			| Self::Graph(_)
			| Self::Closure(_) => true,
			Self::Error(_) | Self::Cursor(_) => true,
		}
	}

	/// Returns the bool payload when this value has the matching variant.
	pub const fn as_bool(&self) -> Option<bool> {
		match self {
			Self::Bool(value) => Some(*value),
			_ => None,
		}
	}

	/// Returns the number payload when this value has the matching variant.
	pub const fn as_number(&self) -> Option<Number> {
		match self {
			Self::Number(value) => Some(*value),
			_ => None,
		}
	}

	/// Borrows str for `Value`.
	pub fn as_str(&self) -> Option<&str> {
		self.as_text().map(Text::as_str)
	}

	/// Borrows text for `Value`.
	pub const fn as_text(&self) -> Option<&Text> {
		match self {
			Self::String(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows array for `Value`.
	pub const fn as_array(&self) -> Option<&Array> {
		match self {
			Self::Array(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows set for `Value`.
	pub const fn as_set(&self) -> Option<&Set> {
		match self {
			Self::Set(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows object for `Value`.
	pub const fn as_object(&self) -> Option<&Object> {
		match self {
			Self::Object(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows duration for `Value`.
	pub const fn as_duration(&self) -> Option<&Duration> {
		match self {
			Self::Duration(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows datetime for `Value`.
	pub const fn as_datetime(&self) -> Option<&DateTime> {
		match self {
			Self::DateTime(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows ulid for `Value`.
	pub const fn as_ulid(&self) -> Option<&Ulid> {
		match self {
			Self::Ulid(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows uuid for `Value`.
	pub const fn as_uuid(&self) -> Option<&Uuid> {
		match self {
			Self::Uuid(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows geometry for `Value`.
	pub const fn as_geometry(&self) -> Option<&Geometry> {
		match self {
			Self::Geometry(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows vector for `Value`.
	pub const fn as_vector(&self) -> Option<&Vector> {
		match self {
			Self::Vector(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows bytes for `Value`.
	pub const fn as_bytes(&self) -> Option<&Bytes> {
		match self {
			Self::Bytes(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows record for `Value`.
	pub const fn as_record(&self) -> Option<&RecordId> {
		match self {
			Self::Record(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows graph ref for `Value`.
	pub const fn as_graph_ref(&self) -> Option<&GraphRef> {
		match self {
			Self::GraphRef(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows regex for `Value`.
	pub const fn as_regex(&self) -> Option<&Regex> {
		match self {
			Self::Regex(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows matcher for `Value`.
	pub const fn as_matcher(&self) -> Option<&Matcher> {
		match self {
			Self::Matcher(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows range for `Value`.
	pub const fn as_range(&self) -> Option<&Range> {
		match self {
			Self::Range(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows atom for `Value`.
	pub fn as_atom(&self) -> Option<&AtomRef> {
		match self {
			Self::Atom(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows atom occurrence for `Value`.
	pub fn as_atom_occurrence(&self) -> Option<&AtomOccurrenceRef> {
		match self {
			Self::AtomOccurrence(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows path for `Value`.
	pub fn as_path(&self) -> Option<&Path> {
		match self {
			Self::Path(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows materialized graph for `Value`.
	pub fn as_materialized_graph(&self) -> Option<&MaterializedGraph> {
		match self {
			Self::Graph(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows graph for `Value`.
	pub fn as_graph(&self) -> Option<&Graph<Frozen>> {
		self.as_materialized_graph().and_then(MaterializedGraph::as_general)
	}

	/// Borrows closure for `Value`.
	pub const fn as_closure(&self) -> Option<&Closure> {
		match self {
			Self::Closure(value) => Some(value),
			_ => None,
		}
	}

	/// Borrows error for `Value`.
	pub const fn as_error(&self) -> Option<&ErrorValue> {
		match self {
			Self::Error(value) => Some(value),
			_ => None,
		}
	}

	/// Returns the cursor payload when this value has the matching variant.
	pub const fn as_cursor(&self) -> Option<QueryCursor> {
		match self {
			Self::Cursor(value) => Some(*value),
			_ => None,
		}
	}

	/// Reports whether the value is storable for `Value`.
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
			| Self::Matcher(_)
			| Self::AtomOccurrence(_)
			| Self::Path(_)
			| Self::Graph(_)
			| Self::Closure(_)
			| Self::Error(_)
			| Self::Cursor(_) => false,
			_ => true,
		}
	}

	/// Ensures storable for `Value`.
	///
	/// # Errors
	///
	/// Returns `conversion_error` diagnostics.
	pub fn ensure_storable(&self) -> Result<(), FerrosGrynnError> {
		self.is_storable()
			.then_some(())
			.ok_or_else(|| FerrosGrynnError::conversion_error(self.kind().name(), "StorageValue"))
	}
}

/// Returns the bound value result.
fn bound_value(bound: Bound<&Value>) -> Option<&Value> {
	match bound {
		Bound::Included(value) | Bound::Excluded(value) => Some(value),
		Bound::Unbounded => None,
	}
}

/// Returns the estimated values heap bytes result.
fn estimated_values_heap_bytes<'a>(
	values: impl Iterator<Item = &'a Value>,
	length: usize,
) -> usize {
	length.saturating_mul(size_of::<Value>()).saturating_add(
		values.map(Value::estimated_heap_bytes).fold(0_usize, usize::saturating_add),
	)
}

/// Reserves complete B-tree nodes, including sparse singleton containers.
fn estimated_tree_heap_bytes<Entry>(count: usize) -> usize {
	crate::memory::tree_retained_bytes::<Entry>(count).unwrap_or(usize::MAX)
}

/// Returns the estimated record id heap bytes result.
pub fn estimated_record_id_heap_bytes(record: &RecordId) -> usize {
	record.estimated_heap_bytes()
}

/// Returns the estimated record part heap bytes result.
pub(super) fn estimated_record_part_heap_bytes(part: &RecordIdPart) -> usize {
	match part {
		RecordIdPart::Int(_) | RecordIdPart::Uuid(_) | RecordIdPart::Ulid(_) => 0,
		RecordIdPart::String(value) => value.estimated_heap_bytes(),
		RecordIdPart::Array(values) => {
			estimated_values_heap_bytes(values.iter(), values.capacity())
		}
		RecordIdPart::Object(values) => values
			.iter()
			.map(|(key, value)| {
				key.estimated_heap_bytes().saturating_add(value.estimated_heap_bytes())
			})
			.fold(estimated_tree_heap_bytes::<(Text, Value)>(values.len()), usize::saturating_add),
	}
}

/// Returns the estimated bound heap bytes result.
fn estimated_bound_heap_bytes(bound: Bound<&Value>) -> usize {
	match bound {
		Bound::Included(value) | Bound::Excluded(value) => {
			size_of::<Value>().saturating_add(value.estimated_heap_bytes())
		}
		Bound::Unbounded => 0,
	}
}

/// Returns the estimated position heap bytes result.
fn estimated_position_heap_bytes(position: &Position) -> usize {
	position.estimated_heap_bytes()
}

/// Returns the estimated positions heap bytes result.
fn estimated_positions_heap_bytes(positions: &[Position], capacity: usize) -> usize {
	capacity.saturating_mul(size_of::<Position>()).saturating_add(
		positions.iter().map(estimated_position_heap_bytes).fold(0_usize, usize::saturating_add),
	)
}

/// Returns the estimated geometry heap bytes result.
pub(super) fn estimated_geometry_heap_bytes(geometry: &Geometry) -> usize {
	match geometry {
		Geometry::Point(position) => estimated_position_heap_bytes(position),
		Geometry::MultiPoint(positions) | Geometry::LineString(positions) => {
			estimated_positions_heap_bytes(positions, positions.capacity())
		}
		Geometry::MultiLineString(lines) | Geometry::Polygon(lines) => {
			lines.capacity().saturating_mul(size_of::<Vec<Position>>()).saturating_add(
				lines
					.iter()
					.map(|line| estimated_positions_heap_bytes(line, line.capacity()))
					.fold(0_usize, usize::saturating_add),
			)
		}
		Geometry::MultiPolygon(polygons) => {
			polygons.capacity().saturating_mul(size_of::<Vec<Vec<Position>>>()).saturating_add(
				polygons
					.iter()
					.map(|polygon| {
						polygon
							.capacity()
							.saturating_mul(size_of::<Vec<Position>>())
							.saturating_add(
								polygon
									.iter()
									.map(|ring| {
										estimated_positions_heap_bytes(ring, ring.capacity())
									})
									.fold(0_usize, usize::saturating_add),
							)
					})
					.fold(0_usize, usize::saturating_add),
			)
		}
		Geometry::Collection(values) => values
			.iter()
			.map(estimated_geometry_heap_bytes)
			.fold(values.capacity().saturating_mul(size_of::<Geometry>()), usize::saturating_add),
	}
}

/// Returns the estimated vector heap bytes result.
fn estimated_vector_heap_bytes(vector: &Vector) -> usize {
	match vector {
		Vector::F32(values) => values.capacity().saturating_mul(size_of::<f32>()),
		Vector::F64(values) => values.capacity().saturating_mul(size_of::<f64>()),
		Vector::F16(values) => values.capacity().saturating_mul(size_of::<half::f16>()),
		Vector::I16(values) => values.capacity().saturating_mul(size_of::<i16>()),
		Vector::I32(values) => values.capacity().saturating_mul(size_of::<i32>()),
		Vector::I64(values) => values.capacity().saturating_mul(size_of::<i64>()),
		Vector::I8(values) => values.capacity().saturating_mul(size_of::<i8>()),
		Vector::U8(values) => values.capacity().saturating_mul(size_of::<u8>()),
	}
}

impl PartialEq for Value {
	/// Compares this `Value` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for Value {}

impl PartialOrd for Value {
	/// Compares this `Value` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Value {
	/// Compares this `Value` with another value using its ordering contract.
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
				(Self::Matcher(left), Self::Matcher(right)) => left.cmp(right),
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
	/// Feeds this `Value` into the supplied hasher.
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
			Self::Matcher(value) => value.hash(state),
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
