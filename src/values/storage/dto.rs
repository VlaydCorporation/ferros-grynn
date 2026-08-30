use std::ops::{Bound, Deref};

use jiff::{Span, Timestamp};
use rust_decimal::Decimal;

use super::{
	DecodeContext,
	DiskAtomRef,
	DiskGraphRef,
	EncodeContext,
	QualifiedAtomRef,
	canonical_f64,
	codec,
	unresolved,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{
		Array,
		Bytes,
		DateTime,
		Duration,
		Geometry,
		GraphRef,
		Number,
		Object,
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
	},
};

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

	pub(super) fn into_runtime(self) -> FerrosGrynnResult<Decimal> {
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

	pub(super) fn to_span(&self) -> FerrosGrynnResult<Span> {
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
	pub(crate) fn from_runtime(
		value: RecordId,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<Self> {
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
