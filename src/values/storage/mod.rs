#![allow(
	dead_code,
	reason = "the storage adapter is internal until the page and columnar integration stage"
)]
#![allow(
	clippy::missing_docs_in_private_items,
	reason = "the crate-private storage DTO is documented as one format contract; field-level \
	          layout is defined by disk_storage_spec"
)]

//! Crate-private storage DTOs and runtime conversion contexts.

pub mod codec;

use std::{
	collections::BTreeMap,
	fmt::{self, Display, Formatter},
	ops::{Bound, Deref},
};

use jiff::{
	Span,
	Timestamp,
	tz::{Offset, TimeZone},
};
use rust_decimal::Decimal;

use super::runtime::{
	Array,
	AtomKind,
	AtomRef,
	Bytes,
	DateTime,
	Duration,
	Geometry,
	GraphRef,
	Number,
	Object,
	PropertyDictionary,
	PropertyKeyId,
	Range,
	RecordId,
	RecordIdPart,
	Set,
	Text,
	Ulid,
	Uuid,
	Value,
	Vector,
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

const DISK_ATOM_KIND_BIT: u64 = 1 << 40;
const DISK_ATOM_SLOT_MASK: u64 = DISK_ATOM_KIND_BIT - 1;
const DISK_ATOM_ALLOWED_MASK: u64 = (DISK_ATOM_KIND_BIT << 1) - 1;
const FIRST_FIXED_TIME_ZONE_ID: u32 = 1_000_000;
const FIXED_TIME_ZONE_BIAS: i32 = 86_399;
const MAX_FIXED_TIME_ZONE_OFFSET: i32 = 86_400;
const MAX_IANA_TIME_ZONE_ID: u32 = FIRST_FIXED_TIME_ZONE_ID - 1;

/// Stable tags of canonical storage format version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TypeTag {
	None = 0x00,
	Null = 0x01,
	False = 0x02,
	True = 0x03,
	Int = 0x04,
	Float = 0x05,
	Decimal = 0x06,
	String = 0x07,
	Bytes = 0x08,
	DateTime = 0x09,
	Duration = 0x0A,
	Uuid = 0x0B,
	Ulid = 0x0C,
	RecordId = 0x10,
	Array = 0x11,
	Set = 0x12,
	Object = 0x13,
	Range = 0x15,
	Point = 0x21,
	LineString = 0x22,
	Polygon = 0x23,
	MultiPoint = 0x24,
	MultiLineString = 0x25,
	MultiPolygon = 0x26,
	GeometryCollection = 0x27,
	VectorF64 = 0x31,
	VectorF32 = 0x32,
	VectorI64 = 0x33,
	VectorI32 = 0x34,
	VectorI16 = 0x35,
	AtomRef = 0x36,
	GraphRef = 0x37,
	QualifiedAtomRef = 0x38,
}

impl TypeTag {
	pub const fn byte(self) -> u8 {
		self as u8
	}
}

/// Generation-free atom address stored on disk.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct DiskAtomRef(u64);

impl DiskAtomRef {
	pub fn from_parts(kind: AtomKind, slot: u64) -> FerrosGrynnResult<Self> {
		if slot > DISK_ATOM_SLOT_MASK {
			return Err(FerrosGrynnError::graph_capacity_exceeded(kind.to_string()));
		}
		let kind = match kind {
			AtomKind::Node => 0,
			AtomKind::Edge => DISK_ATOM_KIND_BIT,
		};
		Ok(Self(kind | slot))
	}

	pub fn from_raw(raw: u64) -> FerrosGrynnResult<Self> {
		if raw & !DISK_ATOM_ALLOWED_MASK != 0 {
			return Err(FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"DiskAtomRef uses reserved high bits",
			));
		}
		Ok(Self(raw))
	}

	pub const fn raw(self) -> u64 {
		self.0
	}

	pub const fn kind(self) -> AtomKind {
		if self.0 & DISK_ATOM_KIND_BIT == 0 {
			AtomKind::Node
		} else {
			AtomKind::Edge
		}
	}

	pub const fn slot(self) -> u64 {
		self.0 & DISK_ATOM_SLOT_MASK
	}
}

impl Display for DiskAtomRef {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{}:{}", self.kind(), self.slot())
	}
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct DiskGraphRef {
	pub(super) slot: u32,
	pub(super) generation: u32,
}

impl DiskGraphRef {
	pub const fn new(slot: u32, generation: u32) -> Self {
		Self {
			slot,
			generation,
		}
	}
}

impl Display for DiskGraphRef {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{}:{}", self.slot, self.generation)
	}
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct QualifiedAtomRef {
	pub(super) graph_slot: u32,
	pub(super) local_ref: DiskAtomRef,
}

impl QualifiedAtomRef {
	pub const fn new(graph_slot: u32, local_ref: DiskAtomRef) -> Self {
		Self {
			graph_slot,
			local_ref,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StorageDecimal {
	pub(super) mantissa: i128,
	pub(super) scale: u8,
}

impl StorageDecimal {
	fn from_runtime(value: Decimal) -> Self {
		Self {
			mantissa: value.mantissa(),
			scale: value.scale() as u8,
		}
	}

	fn into_runtime(self) -> FerrosGrynnResult<Decimal> {
		Decimal::try_from_i128_with_scale(self.mantissa, u32::from(self.scale)).map_err(|source| {
			FerrosGrynnError::decimal_precision_exceeded(
				self.mantissa.to_string(),
				u64::from(self.scale),
			)
			.with_technical_source(source)
		})
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StorageDateTime {
	pub(super) seconds: i64,
	pub(super) nanos: u32,
	pub(super) time_zone_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageDuration {
	pub(super) negative: bool,
	pub(super) units: [u64; 10],
}

impl StorageDuration {
	fn from_runtime(value: Duration) -> Self {
		let span = value.into_inner();
		let signed = [
			i64::from(span.get_years()),
			i64::from(span.get_months()),
			i64::from(span.get_weeks()),
			i64::from(span.get_days()),
			i64::from(span.get_hours()),
			span.get_minutes(),
			span.get_seconds(),
			span.get_milliseconds(),
			span.get_microseconds(),
			span.get_nanoseconds(),
		];
		Self {
			negative: span.signum() < 0,
			units: signed.map(i64::unsigned_abs),
		}
	}

	fn to_span(&self) -> FerrosGrynnResult<Span> {
		let mut signed = [0_i64; 10];
		for (target, magnitude) in signed.iter_mut().zip(self.units) {
			let magnitude = i64::try_from(magnitude).map_err(|_err| {
				FerrosGrynnError::invalid_storage_value_encoding(0, "duration unit exceeds i64")
			})?;
			*target = if self.negative {
				-magnitude
			} else {
				magnitude
			};
		}

		let span = Span::new()
			.try_years(signed[0])
			.and_then(|span| span.try_months(signed[1]))
			.and_then(|span| span.try_weeks(signed[2]))
			.and_then(|span| span.try_days(signed[3]))
			.and_then(|span| span.try_hours(signed[4]))
			.and_then(|span| span.try_minutes(signed[5]))
			.and_then(|span| span.try_seconds(signed[6]))
			.and_then(|span| span.try_milliseconds(signed[7]))
			.and_then(|span| span.try_microseconds(signed[8]))
			.and_then(|span| span.try_nanoseconds(signed[9]));
		span.map_err(|source| {
			FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"duration components are outside jiff::Span limits",
			)
			.with_technical_source(source)
		})
	}

	fn into_runtime(self) -> FerrosGrynnResult<Duration> {
		self.to_span().map(Duration::new)
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageRecordValue {
	Int(i64),
	String(Text),
	Uuid(Uuid),
	Ulid(Ulid),
	Array(Vec<Self>),
	Object(Vec<(Text, Self)>),
}

impl StorageRecordValue {
	fn from_runtime(value: Value) -> FerrosGrynnResult<Self> {
		match value {
			Value::Number(Number::Int(value)) => Ok(Self::Int(value)),
			Value::String(value) => Ok(Self::String(value)),
			Value::Uuid(value) => Ok(Self::Uuid(value)),
			Value::Ulid(value) => Ok(Self::Ulid(value)),
			Value::Array(values) => values
				.into_iter()
				.map(Self::from_runtime)
				.collect::<FerrosGrynnResult<Vec<_>>>()
				.map(Self::Array),
			Value::Object(values) => values
				.into_iter()
				.map(|(key, value)| Self::from_runtime(value).map(|value| (key, value)))
				.collect::<FerrosGrynnResult<Vec<_>>>()
				.map(Self::Object),
			value => Err(FerrosGrynnError::unsupported_storage_value(value.kind().to_string())),
		}
	}

	fn into_runtime(self) -> Value {
		match self {
			Self::Int(value) => Value::from(value),
			Self::String(value) => Value::String(value),
			Self::Uuid(value) => Value::Uuid(value),
			Self::Ulid(value) => Value::Ulid(value),
			Self::Array(values) => {
				Value::Array(Array::new(values.into_iter().map(Self::into_runtime).collect()))
			}
			Self::Object(values) => Value::Object(
				values.into_iter().map(|(key, value)| (key, value.into_runtime())).collect(),
			),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageRecordIdPart {
	Int(i64),
	String(Text),
	Uuid(Uuid),
	Ulid(Ulid),
	Array(Vec<StorageRecordValue>),
	Object(Vec<(Text, StorageRecordValue)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRecordId {
	pub(super) table_id: u32,
	pub(super) part: StorageRecordIdPart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageBound {
	Unbounded,
	Included(Box<StorageValue>),
	Excluded(Box<StorageValue>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRange {
	pub(super) start: StorageBound,
	pub(super) end: StorageBound,
}

/// Persistable, dictionary-aware representation used by the canonical codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageValue {
	None,
	Null,
	Bool(bool),
	Int(i64),
	Float(u64),
	Decimal(StorageDecimal),
	String(Text),
	Bytes(Bytes),
	DateTime(StorageDateTime),
	Duration(StorageDuration),
	Uuid(Uuid),
	Ulid(Ulid),
	Record(StorageRecordId),
	Array(Vec<Self>),
	Set(Vec<Self>),
	Object(Vec<(PropertyKeyId, Self)>),
	Range(StorageRange),
	Geometry(Geometry),
	Vector(Vector),
	AtomRef(DiskAtomRef),
	GraphRef(DiskGraphRef),
	QualifiedAtomRef(QualifiedAtomRef),
}

impl StorageValue {
	pub fn from_runtime(value: Value, context: &mut EncodeContext) -> FerrosGrynnResult<Self> {
		match value {
			Value::None => Ok(Self::None),
			Value::Null => Ok(Self::Null),
			Value::Bool(value) => Ok(Self::Bool(value)),
			Value::Number(Number::Int(value)) => Ok(Self::Int(value)),
			Value::Number(Number::Float(value)) => Ok(Self::Float(canonical_f64(value))),
			Value::Number(Number::Decimal(value)) => {
				Ok(Self::Decimal(StorageDecimal::from_runtime(value)))
			}
			Value::String(value) => Ok(Self::String(value)),
			Value::Bytes(value) => Ok(Self::Bytes(value)),
			Value::DateTime(value) => {
				StorageDateTime::from_runtime(value, context).map(Self::DateTime)
			}
			Value::Duration(value) => Ok(Self::Duration(StorageDuration::from_runtime(value))),
			Value::Uuid(value) => Ok(Self::Uuid(value)),
			Value::Ulid(value) => Ok(Self::Ulid(value)),
			Value::Record(value) => {
				StorageRecordId::from_runtime(*value, context).map(Self::Record)
			}
			Value::Array(values) => values
				.into_iter()
				.map(|value| Self::from_runtime(value, context))
				.collect::<FerrosGrynnResult<Vec<_>>>()
				.map(Self::Array),
			Value::Set(values) => {
				let mut values = values
					.into_iter()
					.map(|value| Self::from_runtime(value, context))
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				codec::canonicalize_set(&mut values)?;
				Ok(Self::Set(values))
			}
			Value::Object(values) => {
				let mut result = Vec::with_capacity(values.len());
				for (key, value) in values {
					let key = context.properties.intern(key)?;
					result.push((key, Self::from_runtime(value, context)?));
				}
				result.sort_unstable_by_key(|(key, _value)| *key);
				Ok(Self::Object(result))
			}
			Value::Range(value) => StorageRange::from_runtime(value, context).map(Self::Range),
			Value::Geometry(value) => Ok(Self::Geometry(value)),
			Value::Vector(value) => Ok(Self::Vector(value)),
			Value::Atom(value) => context.atom_to_disk(&value).map(Self::AtomRef),
			Value::Regex(_) | Value::Path(_) | Value::Graph(_) | Value::Closure(_) => {
				Err(FerrosGrynnError::unsupported_storage_value(value.kind().to_string()))
			}
		}
	}

	pub fn from_graph_ref(value: &GraphRef, context: &EncodeContext) -> FerrosGrynnResult<Self> {
		context.graph_to_disk(value).map(Self::GraphRef)
	}

	pub fn into_runtime(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<Value> {
		match self {
			Self::None => Ok(Value::None),
			Self::Null => Ok(Value::Null),
			Self::Bool(value) => Ok(Value::Bool(value)),
			Self::Int(value) => Ok(Value::from(value)),
			Self::Float(value) => Ok(Value::from(f64::from_bits(value))),
			Self::Decimal(value) => value.into_runtime().map(Value::from),
			Self::String(value) => Ok(Value::String(value)),
			Self::Bytes(value) => Ok(Value::Bytes(value)),
			Self::DateTime(value) => value.into_runtime(context).map(Value::DateTime),
			Self::Duration(value) => value.into_runtime().map(Value::Duration),
			Self::Uuid(value) => Ok(Value::Uuid(value)),
			Self::Ulid(value) => Ok(Value::Ulid(value)),
			Self::Record(value) => value.into_runtime(context).map(Value::from),
			Self::Array(values) => values
				.into_iter()
				.map(|value| value.into_runtime(context))
				.collect::<FerrosGrynnResult<Vec<_>>>()
				.map(Array::new)
				.map(Value::Array),
			Self::Set(values) => {
				let mut result = Set::new();
				for value in values {
					if !result.insert(value.into_runtime(context)?) {
						return Err(FerrosGrynnError::invalid_storage_value_encoding(
							0,
							"set contains values equal under runtime semantics",
						));
					}
				}
				Ok(Value::Set(result))
			}
			Self::Object(values) => {
				let mut result = Object::new();
				for (key, value) in values {
					let key = context
						.properties
						.resolve(key)
						.cloned()
						.ok_or_else(|| unresolved("property key", key.to_string()))?;
					result.insert(key, value.into_runtime(context)?);
				}
				Ok(Value::Object(result))
			}
			Self::Range(value) => value.into_runtime(context).map(Value::Range),
			Self::Geometry(value) => Ok(Value::Geometry(value)),
			Self::Vector(value) => Ok(Value::Vector(value)),
			Self::AtomRef(value) => context.atom_from_disk(value).cloned().map(Value::from),
			Self::GraphRef(_) | Self::QualifiedAtomRef(_) => {
				Err(FerrosGrynnError::unsupported_storage_value("graph reference"))
			}
		}
	}

	pub fn into_graph_ref(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<GraphRef> {
		match self {
			Self::GraphRef(value) => context.graph_from_disk(value).cloned(),
			value => Err(FerrosGrynnError::conversion_error(value.type_name(), "GraphRef")),
		}
	}

	pub const fn as_ref(&self) -> StorageValueRef<'_> {
		StorageValueRef(self)
	}

	fn type_name(&self) -> &'static str {
		match self {
			Self::None => "none",
			Self::Null => "null",
			Self::Bool(_) => "bool",
			Self::Int(_) => "int",
			Self::Float(_) => "float",
			Self::Decimal(_) => "decimal",
			Self::String(_) => "string",
			Self::Bytes(_) => "bytes",
			Self::DateTime(_) => "datetime",
			Self::Duration(_) => "duration",
			Self::Uuid(_) => "uuid",
			Self::Ulid(_) => "ulid",
			Self::Record(_) => "record_id",
			Self::Array(_) => "array",
			Self::Set(_) => "set",
			Self::Object(_) => "object",
			Self::Range(_) => "range",
			Self::Geometry(_) => "geometry",
			Self::Vector(_) => "vector",
			Self::AtomRef(_) => "atom_ref",
			Self::GraphRef(_) => "graph_ref",
			Self::QualifiedAtomRef(_) => "qualified_atom_ref",
		}
	}
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct StorageValueRef<'a>(&'a StorageValue);

impl<'a> StorageValueRef<'a> {
	pub const fn new(value: &'a StorageValue) -> Self {
		Self(value)
	}
}

impl Deref for StorageValueRef<'_> {
	type Target = StorageValue;

	fn deref(&self) -> &Self::Target {
		self.0
	}
}

impl StorageDateTime {
	fn from_runtime(value: DateTime, context: &mut EncodeContext) -> FerrosGrynnResult<Self> {
		let zoned = value.into_inner();
		let timestamp = zoned.timestamp();
		let nanoseconds = timestamp.as_nanosecond();
		let seconds = nanoseconds.div_euclid(1_000_000_000) as i64;
		let nanos = nanoseconds.rem_euclid(1_000_000_000) as u32;
		let time_zone_id = context.intern_time_zone(&zoned)?;
		Ok(Self {
			seconds,
			nanos,
			time_zone_id,
		})
	}

	fn into_runtime(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<DateTime> {
		if self.nanos >= 1_000_000_000 {
			return Err(FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"datetime nanoseconds must be below one second",
			));
		}
		let timestamp = Timestamp::new(self.seconds, self.nanos as i32).map_err(|source| {
			FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"datetime timestamp is outside jiff limits",
			)
			.with_technical_source(source)
		})?;
		let time_zone = context.time_zone(self.time_zone_id)?;
		Ok(DateTime::new(timestamp.to_zoned(time_zone)))
	}
}

impl StorageRecordId {
	fn from_runtime(value: RecordId, context: &mut EncodeContext) -> FerrosGrynnResult<Self> {
		let (table, part) = value.into_parts();
		let table_id = context.tables.intern(table, 0, u32::MAX)?;
		let part = match part {
			RecordIdPart::Int(value) => StorageRecordIdPart::Int(value),
			RecordIdPart::String(value) => StorageRecordIdPart::String(value),
			RecordIdPart::Uuid(value) => StorageRecordIdPart::Uuid(value),
			RecordIdPart::Ulid(value) => StorageRecordIdPart::Ulid(value),
			RecordIdPart::Array(values) => StorageRecordIdPart::Array(
				values
					.into_iter()
					.map(StorageRecordValue::from_runtime)
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
			RecordIdPart::Object(values) => StorageRecordIdPart::Object(
				values
					.into_iter()
					.map(|(key, value)| {
						StorageRecordValue::from_runtime(value).map(|value| (key, value))
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
		};
		Ok(Self {
			table_id,
			part,
		})
	}

	fn into_runtime(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<RecordId> {
		let table = context
			.tables
			.resolve(self.table_id, 0)
			.cloned()
			.ok_or_else(|| unresolved("table", self.table_id.to_string()))?;
		let part = match self.part {
			StorageRecordIdPart::Int(value) => RecordIdPart::Int(value),
			StorageRecordIdPart::String(value) => RecordIdPart::String(value),
			StorageRecordIdPart::Uuid(value) => RecordIdPart::Uuid(value),
			StorageRecordIdPart::Ulid(value) => RecordIdPart::Ulid(value),
			StorageRecordIdPart::Array(values) => RecordIdPart::Array(Array::new(
				values.into_iter().map(StorageRecordValue::into_runtime).collect(),
			)),
			StorageRecordIdPart::Object(values) => RecordIdPart::Object(
				values.into_iter().map(|(key, value)| (key, value.into_runtime())).collect(),
			),
		};
		RecordId::new(table, part)
	}
}

impl StorageRange {
	fn from_runtime(value: Range, context: &mut EncodeContext) -> FerrosGrynnResult<Self> {
		let (start, end) = value.into_bounds();
		Ok(Self {
			start: StorageBound::from_runtime(start, context)?,
			end: StorageBound::from_runtime(end, context)?,
		})
	}

	fn into_runtime(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<Range> {
		Range::new(self.start.into_runtime(context)?, self.end.into_runtime(context)?)
	}
}

impl StorageBound {
	fn from_runtime(value: Bound<Value>, context: &mut EncodeContext) -> FerrosGrynnResult<Self> {
		match value {
			Bound::Unbounded => Ok(Self::Unbounded),
			Bound::Included(value) => {
				StorageValue::from_runtime(value, context).map(Box::new).map(Self::Included)
			}
			Bound::Excluded(value) => {
				StorageValue::from_runtime(value, context).map(Box::new).map(Self::Excluded)
			}
		}
	}

	fn into_runtime(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<Bound<Value>> {
		match self {
			Self::Unbounded => Ok(Bound::Unbounded),
			Self::Included(value) => value.into_runtime(context).map(Bound::Included),
			Self::Excluded(value) => value.into_runtime(context).map(Bound::Excluded),
		}
	}
}

#[derive(Debug, Default)]
struct StringDictionary {
	ids: BTreeMap<Text, u32>,
	names: Vec<Text>,
}

impl StringDictionary {
	fn intern(&mut self, name: Text, first_id: u32, maximum_id: u32) -> FerrosGrynnResult<u32> {
		if let Some(id) = self.ids.get(name.as_str()) {
			return Ok(*id);
		}
		let index = u32::try_from(self.names.len()).map_err(|_err| {
			FerrosGrynnError::storage_decode_limit_exceeded(
				"dictionary entries",
				u64::from(maximum_id),
				u64::MAX,
			)
		})?;
		let id = first_id.checked_add(index).filter(|id| *id <= maximum_id).ok_or_else(|| {
			FerrosGrynnError::storage_decode_limit_exceeded(
				"dictionary entries",
				u64::from(maximum_id - first_id + 1),
				self.names.len() as u64 + 1,
			)
		})?;
		self.names.push(name.clone());
		self.ids.insert(name, id);
		Ok(id)
	}

	fn resolve(&self, id: u32, first_id: u32) -> Option<&Text> {
		id.checked_sub(first_id).and_then(|index| self.names.get(index as usize))
	}
}

/// Mutable dictionary and reference context used while converting runtime values.
#[derive(Debug, Default)]
pub struct EncodeContext {
	properties: PropertyDictionary,
	tables: StringDictionary,
	time_zones: StringDictionary,
	atoms_to_disk: BTreeMap<AtomRef, DiskAtomRef>,
	atoms_from_disk: BTreeMap<DiskAtomRef, AtomRef>,
	graphs_to_disk: BTreeMap<GraphRef, DiskGraphRef>,
	graphs_from_disk: BTreeMap<DiskGraphRef, GraphRef>,
}

impl EncodeContext {
	pub fn new(properties: PropertyDictionary) -> Self {
		Self {
			properties,
			..Self::default()
		}
	}

	pub const fn property_dictionary(&self) -> &PropertyDictionary {
		&self.properties
	}

	pub fn register_atom(&mut self, runtime: AtomRef, disk: DiskAtomRef) -> FerrosGrynnResult<()> {
		if runtime.kind() != disk.kind()
			|| self.atoms_to_disk.get(&runtime).is_some_and(|existing| *existing != disk)
			|| self.atoms_from_disk.get(&disk).is_some_and(|existing| existing != &runtime)
		{
			return Err(FerrosGrynnError::storage_context_conflict("atom", disk.to_string()));
		}
		self.atoms_to_disk.insert(runtime.clone(), disk);
		self.atoms_from_disk.insert(disk, runtime);
		Ok(())
	}

	pub fn register_graph(
		&mut self,
		runtime: GraphRef,
		disk: DiskGraphRef,
	) -> FerrosGrynnResult<()> {
		if self.graphs_to_disk.get(&runtime).is_some_and(|existing| *existing != disk)
			|| self.graphs_from_disk.get(&disk).is_some_and(|existing| existing != &runtime)
		{
			return Err(FerrosGrynnError::storage_context_conflict("graph", disk.to_string()));
		}
		self.graphs_to_disk.insert(runtime.clone(), disk);
		self.graphs_from_disk.insert(disk, runtime);
		Ok(())
	}

	pub const fn decoder(&self) -> DecodeContext<'_> {
		DecodeContext {
			properties: &self.properties,
			tables: &self.tables,
			time_zones: &self.time_zones,
			atoms: &self.atoms_from_disk,
			graphs: &self.graphs_from_disk,
		}
	}

	fn atom_to_disk(&self, value: &AtomRef) -> FerrosGrynnResult<DiskAtomRef> {
		self.atoms_to_disk
			.get(value)
			.copied()
			.ok_or_else(|| unresolved("atom", value.id().to_string()))
	}

	fn graph_to_disk(&self, value: &GraphRef) -> FerrosGrynnResult<DiskGraphRef> {
		self.graphs_to_disk
			.get(value)
			.copied()
			.ok_or_else(|| unresolved("graph", value.id().to_string()))
	}

	fn intern_time_zone(&mut self, value: &jiff::Zoned) -> FerrosGrynnResult<u32> {
		if let Some(name) = value.time_zone().iana_name() {
			if name == "UTC" {
				return Ok(0);
			}
			return self.time_zones.intern(name.into(), 1, MAX_IANA_TIME_ZONE_ID);
		}

		let offset = value.offset().seconds();
		if !(-FIXED_TIME_ZONE_BIAS..=MAX_FIXED_TIME_ZONE_OFFSET).contains(&offset) {
			return Err(FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"fixed time-zone offset is outside the storage range",
			));
		}
		Ok((i64::from(FIRST_FIXED_TIME_ZONE_ID)
			+ i64::from(offset)
			+ i64::from(FIXED_TIME_ZONE_BIAS)) as u32)
	}
}

/// Borrowed resolver context used while moving storage values into runtime
/// values.
#[derive(Debug, Clone, Copy)]
pub struct DecodeContext<'a> {
	properties: &'a PropertyDictionary,
	tables: &'a StringDictionary,
	time_zones: &'a StringDictionary,
	atoms: &'a BTreeMap<DiskAtomRef, AtomRef>,
	graphs: &'a BTreeMap<DiskGraphRef, GraphRef>,
}

impl DecodeContext<'_> {
	fn atom_from_disk(&self, value: DiskAtomRef) -> FerrosGrynnResult<&AtomRef> {
		self.atoms.get(&value).ok_or_else(|| unresolved("atom", value.to_string()))
	}

	fn graph_from_disk(&self, value: DiskGraphRef) -> FerrosGrynnResult<&GraphRef> {
		self.graphs.get(&value).ok_or_else(|| unresolved("graph", value.to_string()))
	}

	fn time_zone(&self, id: u32) -> FerrosGrynnResult<TimeZone> {
		match id {
			0 => Ok(TimeZone::UTC),
			1..=MAX_IANA_TIME_ZONE_ID => {
				let name = self
					.time_zones
					.resolve(id, 1)
					.ok_or_else(|| unresolved("time zone", id.to_string()))?;
				TimeZone::get(name.as_str()).map_err(|source| {
					FerrosGrynnError::unresolved_storage_reference("time zone", name.to_string())
						.with_technical_source(source)
				})
			}
			FIRST_FIXED_TIME_ZONE_ID.. => {
				let offset = i64::from(id)
					- i64::from(FIRST_FIXED_TIME_ZONE_ID)
					- i64::from(FIXED_TIME_ZONE_BIAS);
				let offset = i32::try_from(offset).map_err(|_err| {
					FerrosGrynnError::invalid_storage_value_encoding(
						0,
						"fixed time-zone identifier overflows seconds",
					)
				})?;
				if !(-FIXED_TIME_ZONE_BIAS..=MAX_FIXED_TIME_ZONE_OFFSET).contains(&offset) {
					return Err(FerrosGrynnError::invalid_storage_value_encoding(
						0,
						"fixed time-zone identifier is outside the storage range",
					));
				}
				Offset::from_seconds(offset).map(TimeZone::fixed).map_err(|source| {
					FerrosGrynnError::invalid_storage_value_encoding(
						0,
						"fixed time-zone offset is invalid",
					)
					.with_technical_source(source)
				})
			}
		}
	}
}

fn canonical_f64(value: f64) -> u64 {
	if value.is_nan() {
		f64::NAN.to_bits()
	} else if value == 0.0 {
		0.0_f64.to_bits()
	} else {
		value.to_bits()
	}
}

fn unresolved(reference_type: &str, reference: String) -> FerrosGrynnError {
	FerrosGrynnError::unresolved_storage_reference(reference_type, reference)
}

#[cfg(test)]
mod tests {
	use std::ops::Bound;

	use jiff::{
		Timestamp,
		tz::{Offset, TimeZone},
	};

	use super::{
		DiskAtomRef,
		DiskGraphRef,
		EncodeContext,
		StorageDecimal,
		StorageValue,
		codec::{CanonicalCodec, DecodeLimits},
	};
	use crate::values::{
		Array,
		AtomKind,
		AtomRef,
		GraphKind,
		GraphRef,
		GraphStorageMode,
		Object,
		Range,
		RecordId,
		RecordIdPart,
		Set,
		Value,
	};

	#[test]
	fn runtime_storage_conversion_preserves_nested_values_and_dictionaries() {
		let record = RecordId::new(
			"person",
			RecordIdPart::Object(Object::from_iter([("region", Value::from("eu"))])),
		)
		.unwrap();
		let atom = AtomRef::new(AtomKind::Node, record.clone());
		let mut object = Object::new();
		object.insert("name", "Ada");
		object.insert("record", record);
		object.insert("set", Value::Set(Set::from(vec![3_i64, 1, 2])));
		object.insert(
			"range",
			Range::new(Bound::Included(1_i64.into()), Bound::Excluded(4_i64.into())).unwrap(),
		);
		object.insert("atom", atom.clone());

		let mut context = EncodeContext::default();
		context.register_atom(atom, DiskAtomRef::from_parts(AtomKind::Node, 7).unwrap()).unwrap();
		let storage =
			StorageValue::from_runtime(Value::Object(object.clone()), &mut context).unwrap();
		let encoded = CanonicalCodec::encode(storage.as_ref()).unwrap();
		let storage = CanonicalCodec::decode(&encoded, DecodeLimits::default()).unwrap();
		let runtime = storage.into_runtime(&context.decoder()).unwrap();
		assert_eq!(runtime, Value::Object(object));
		assert_eq!(context.property_dictionary().len(), 5);
	}

	#[test]
	fn graph_references_use_explicit_context_mapping() {
		let id = RecordId::new("graph", RecordIdPart::Int(1)).unwrap();
		let graph = GraphRef::new(id, GraphKind::Dag, GraphStorageMode::DiskBacked);
		let disk = DiskGraphRef::new(8, 2);
		let mut context = EncodeContext::default();
		context.register_graph(graph.clone(), disk).unwrap();

		let storage = StorageValue::from_graph_ref(&graph, &context).unwrap();
		assert_eq!(storage.into_graph_ref(&context.decoder()).unwrap(), graph);
	}

	#[test]
	fn rejects_runtime_only_values_and_missing_references() {
		let mut context = EncodeContext::default();
		let regex = "a+".parse::<crate::values::Regex>().unwrap();
		let error = StorageValue::from_runtime(Value::Regex(regex), &mut context).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S05");

		let record = RecordId::new("person", RecordIdPart::Int(1)).unwrap();
		let atom = AtomRef::new(AtomKind::Node, record);
		let error = StorageValue::from_runtime(Value::from(atom), &mut context).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S04");
	}

	#[test]
	fn array_storage_conversion_reuses_owned_elements() {
		let mut context = EncodeContext::default();
		let value = Value::Array(Array::new(vec![
			Value::from("text"),
			Value::Bytes(crate::values::Bytes::from([1_u8, 2])),
		]));
		let storage = StorageValue::from_runtime(value.clone(), &mut context).unwrap();
		assert_eq!(storage.into_runtime(&context.decoder()).unwrap(), value);
	}

	#[test]
	fn datetime_conversion_preserves_pre_epoch_nanos_and_time_zone_kind() {
		let utc =
			"1969-12-31T23:59:59.500000000+00:00[UTC]".parse::<crate::values::DateTime>().unwrap();
		let fixed = crate::values::DateTime::new(
			Timestamp::new(1_700_000_000, 123)
				.unwrap()
				.to_zoned(TimeZone::fixed(Offset::from_seconds(19_800).unwrap())),
		);
		let mut context = EncodeContext::default();

		let utc_storage = StorageValue::from_runtime(Value::DateTime(utc), &mut context).unwrap();
		let StorageValue::DateTime(utc_storage) = utc_storage else {
			panic!("datetime conversion changed the storage variant");
		};
		assert_eq!(
			(utc_storage.seconds, utc_storage.nanos, utc_storage.time_zone_id),
			(-1, 500_000_000, 0),
		);

		let fixed_storage =
			StorageValue::from_runtime(Value::DateTime(fixed.clone()), &mut context).unwrap();
		let fixed_runtime = fixed_storage.into_runtime(&context.decoder()).unwrap();
		let Value::DateTime(fixed_runtime) = fixed_runtime else {
			panic!("datetime decoding changed the runtime variant");
		};
		assert_eq!(fixed_runtime, fixed);
		assert_eq!(fixed_runtime.as_inner().offset().seconds(), 19_800);
	}

	#[test]
	fn wider_storage_decimal_fails_without_precision_loss() {
		let error = StorageDecimal {
			mantissa: i128::MAX,
			scale: 0,
		}
		.into_runtime()
		.unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S03");
		assert!(error.technical_source().is_some());
	}

	#[test]
	fn conversion_context_rejects_conflicting_reference_mappings() {
		let first =
			AtomRef::new(AtomKind::Node, RecordId::new("person", RecordIdPart::Int(1)).unwrap());
		let second =
			AtomRef::new(AtomKind::Node, RecordId::new("person", RecordIdPart::Int(2)).unwrap());
		let disk = DiskAtomRef::from_parts(AtomKind::Node, 7).unwrap();
		let mut context = EncodeContext::default();
		context.register_atom(first, disk).unwrap();
		let error = context.register_atom(second, disk).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S07");
	}

	#[test]
	fn set_conversion_rejects_cross_number_duplicates() {
		let context = EncodeContext::default();
		let value =
			StorageValue::Set(vec![StorageValue::Int(1), StorageValue::Float(1.0_f64.to_bits())]);
		let error = value.into_runtime(&context.decoder()).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22S02");
	}
}
