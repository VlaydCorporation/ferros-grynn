use std::ops::{Bound, Deref};

use fg_runtime::__internal::{GeometryInternal as _, RangeInternal as _};

/// Borrowed conversion with admission before DTO and context allocations.
mod bounded;

use jiff::{Span, Timestamp};
use rust_decimal::Decimal;

use super::{
	DecodeContext,
	DiskGraphRef,
	EncodeContext,
	QualifiedAtomRef,
	canonical_f64,
	codec,
	tree_allocation_bytes,
	unresolved,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{
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

/// Explicit decimal mantissa and scale used by the storage value format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StorageDecimal {
	/// Signed integer coefficient of the stored decimal value.
	pub(super) mantissa: i128,
	/// Number of decimal fractional digits represented by the mantissa.
	pub(super) scale: u8,
}

impl StorageDecimal {
	/// Constructs from runtime for `StorageDecimal`.
	fn from_runtime(value: Decimal) -> Self {
		Self {
			mantissa: value.mantissa(),
			scale: value.scale() as u8,
		}
	}

	/// Converts this value into runtime for `StorageDecimal`.
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

/// Storage timestamp components and interned time-zone identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StorageDateTime {
	/// Whole timestamp seconds retained by the storage representation.
	pub(super) seconds: i64,
	/// Subsecond nanoseconds retained by the storage representation.
	pub(super) nanos: u32,
	/// Interned time-zone identifier resolved through the conversion context.
	pub(super) time_zone_id: u32,
}

/// Storage duration sign and explicit unit counters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageDuration {
	/// Whether the stored duration has a negative sign.
	pub(super) negative: bool,
	/// Explicit duration-unit counters written by the canonical storage codec.
	pub(super) units: [u64; 10],
}

impl StorageDuration {
	/// Constructs from runtime for `StorageDuration`.
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

	/// Converts to span for `StorageDuration`.
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

	/// Converts this value into runtime for `StorageDuration`.
	fn into_runtime(self) -> FerrosGrynnResult<Duration> {
		self.to_span().map(Duration::new)
	}
}

/// Storage-compatible scalar or composite value inside a record key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageRecordValue {
	/// Storage representation of a signed integer.
	Int(
		/// Storage representation of a signed integer.
		i64,
	),
	/// Storage representation of text.
	String(
		/// Storage representation of text.
		Text,
	),
	/// Storage representation of a UUID.
	Uuid(
		/// Storage representation of a UUID.
		Uuid,
	),
	/// Storage representation of a ULID.
	Ulid(
		/// Storage representation of a ULID.
		Ulid,
	),
	/// Storage representation of an ordered sequence of values.
	Array(
		/// Storage representation of an ordered sequence of values.
		Vec<Self>,
	),
	/// Storage representation of a text-keyed value map.
	Object(
		/// Storage representation of a text-keyed value map.
		Vec<(Text, Self)>,
	),
}

impl StorageRecordValue {
	/// Constructs from runtime for `StorageRecordValue`.
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

	/// Converts this value into runtime for `StorageRecordValue`.
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

/// Explicit storage representation of a scalar or composite record key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageRecordIdPart {
	/// Storage representation of a signed integer.
	Int(
		/// Storage representation of a signed integer.
		i64,
	),
	/// Storage representation of text.
	String(
		/// Storage representation of text.
		Text,
	),
	/// Storage representation of a UUID.
	Uuid(
		/// Storage representation of a UUID.
		Uuid,
	),
	/// Storage representation of a ULID.
	Ulid(
		/// Storage representation of a ULID.
		Ulid,
	),
	/// Storage representation of an ordered sequence of values.
	Array(
		/// Storage representation of an ordered sequence of values.
		Vec<StorageRecordValue>,
	),
	/// Storage representation of a text-keyed value map.
	Object(
		/// Storage representation of a text-keyed value map.
		Vec<(Text, StorageRecordValue)>,
	),
}

/// Interned class identifier paired with its encoded record key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRecordId {
	/// Resolved class identifier associated with the stored record.
	pub(super) class_id: u32,
	/// Stable class-qualified record identity used by this operation.
	pub(super) part: StorageRecordIdPart,
}

/// Canonical database-stable atom reference persisted inside a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageAtomRef {
	/// Selected operation or representation discriminator.
	pub(super) kind: AtomKind,
	/// Stable class-qualified record identity used by this operation.
	pub(super) record_id: StorageRecordId,
}

impl StorageAtomRef {
	/// Returns the immutable node or edge kind.
	pub const fn kind(&self) -> AtomKind {
		self.kind
	}

	/// Constructs from runtime for `StorageAtomRef`.
	fn from_runtime(value: AtomRef, context: &EncodeContext) -> FerrosGrynnResult<Self> {
		Ok(Self {
			kind: value.kind(),
			record_id: StorageRecordId::from_runtime(value.id().clone(), context)?,
		})
	}

	/// Converts this value into runtime for `StorageAtomRef`.
	fn into_runtime(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<AtomRef> {
		self.record_id.into_runtime(context).map(|id| AtomRef::new(self.kind, id))
	}
}

/// Unbounded, inclusive or exclusive endpoint of a stored range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageBound {
	/// No finite endpoint is stored.
	Unbounded,
	/// The stored endpoint belongs to the range.
	Included(
		/// The stored endpoint belongs to the range.
		Box<StorageValue>,
	),
	/// The stored endpoint does not belong to the range.
	Excluded(
		/// The stored endpoint does not belong to the range.
		Box<StorageValue>,
	),
}

/// Stored range with independently represented start and end bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRange {
	/// Initial endpoint or lower range bound selected by the syntax element.
	pub(super) start: StorageBound,
	/// Final endpoint or upper range bound selected by the syntax element.
	pub(super) end: StorageBound,
}

/// Persistable, dictionary-aware representation used by the canonical codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageValue {
	/// Storage representation of an absent value.
	None,
	/// Storage representation of an explicit null value.
	Null,
	/// Storage representation of a Boolean value.
	Bool(
		/// Storage representation of a Boolean value.
		bool,
	),
	/// Storage representation of a signed integer.
	Int(
		/// Storage representation of a signed integer.
		i64,
	),
	/// Storage representation of a floating-point number.
	Float(
		/// Storage representation of a floating-point number.
		u64,
	),
	/// Storage representation of a decimal number.
	Decimal(
		/// Storage representation of a decimal number.
		StorageDecimal,
	),
	/// Storage representation of text.
	String(
		/// Storage representation of text.
		Text,
	),
	/// Storage representation of binary bytes.
	Bytes(
		/// Storage representation of binary bytes.
		Bytes,
	),
	/// Storage representation of a time-zone-aware timestamp.
	DateTime(
		/// Storage representation of a time-zone-aware timestamp.
		StorageDateTime,
	),
	/// Storage representation of a duration.
	Duration(
		/// Storage representation of a duration.
		StorageDuration,
	),
	/// Storage representation of a UUID.
	Uuid(
		/// Storage representation of a UUID.
		Uuid,
	),
	/// Storage representation of a ULID.
	Ulid(
		/// Storage representation of a ULID.
		Ulid,
	),
	/// Storage representation of a class-qualified record identity.
	Record(
		/// Storage representation of a class-qualified record identity.
		StorageRecordId,
	),
	/// Storage representation of an ordered sequence of values.
	Array(
		/// Storage representation of an ordered sequence of values.
		Vec<Self>,
	),
	/// Storage representation of an ordered set of distinct values.
	Set(
		/// Storage representation of an ordered set of distinct values.
		Vec<Self>,
	),
	/// Storage representation of a text-keyed value map.
	Object(
		/// Storage representation of a text-keyed value map.
		Vec<(PropertyKeyId, Self)>,
	),
	/// Storage representation of a bounded or unbounded value interval.
	Range(
		/// Storage representation of a bounded or unbounded value interval.
		StorageRange,
	),
	/// Storage representation of a geometry value.
	Geometry(
		/// Storage representation of a geometry value.
		Geometry,
	),
	/// Storage representation of a numeric vector.
	Vector(
		/// Storage representation of a numeric vector.
		Vector,
	),
	/// Storage representation of an atom reference.
	AtomRef(
		/// Storage representation of an atom reference.
		StorageAtomRef,
	),
	/// Storage representation of a stable graph reference.
	GraphRef(
		/// Storage representation of a stable graph reference.
		DiskGraphRef,
	),
	/// Storage representation of a graph-qualified atom reference.
	QualifiedAtomRef(
		/// Storage representation of a graph-qualified atom reference.
		QualifiedAtomRef,
	),
}

impl StorageValue {
	/// Bounds the owned runtime conversion, including dictionary-name clones
	/// that are absent from the canonical decoder's allocation accounting.
	///
	/// # Errors
	/// Rejects unsupported values, unresolved references or exceeded resource
	/// limits.
	pub fn runtime_allocation_bytes(
		&self,
		context: &DecodeContext<'_>,
	) -> FerrosGrynnResult<usize> {
		let mut bytes = 256usize;
		match self {
			Self::Array(values) | Self::Set(values) => {
				if matches!(self, Self::Set(_)) {
					allocation_add(
						&mut bytes,
						tree_allocation_bytes(values.len(), std::mem::size_of::<Value>())?,
					)?;
				}
				for value in values {
					allocation_add(&mut bytes, value.runtime_allocation_bytes(context)?)?;
				}
			}
			Self::Object(values) => {
				allocation_add(
					&mut bytes,
					tree_allocation_bytes(values.len(), std::mem::size_of::<(Text, Value)>())?,
				)?;
				for (key, value) in values {
					let name = context
						.properties
						.resolve(*key)
						.ok_or_else(|| unresolved("property key", key.to_string()))?;
					allocation_add(&mut bytes, name.len())?;
					allocation_add(&mut bytes, value.runtime_allocation_bytes(context)?)?;
				}
			}
			Self::Record(value) => {
				allocation_add(&mut bytes, value.runtime_allocation_bytes(context)?)?;
			}
			Self::AtomRef(value) => {
				allocation_add(&mut bytes, value.record_id.runtime_allocation_bytes(context)?)?;
			}
			Self::GraphRef(value) => {
				allocation_add(&mut bytes, context.graph_from_disk(*value)?.name().len())?;
			}
			Self::Range(value) => {
				for bound in [&value.start, &value.end] {
					if let StorageBound::Included(value) | StorageBound::Excluded(value) = bound {
						allocation_add(&mut bytes, value.runtime_allocation_bytes(context)?)?;
					}
				}
			}
			Self::QualifiedAtomRef(_) => {
				return Err(FerrosGrynnError::unsupported_storage_value(
					"qualified atom reference",
				));
			}
			Self::Geometry(geometry) => {
				allocation_add(&mut bytes, geometry.validation_scratch_bytes()?)?;
			}
			Self::None
			| Self::Null
			| Self::Bool(_)
			| Self::Int(_)
			| Self::Float(_)
			| Self::Decimal(_)
			| Self::String(_)
			| Self::Bytes(_)
			| Self::DateTime(_)
			| Self::Duration(_)
			| Self::Uuid(_)
			| Self::Ulid(_)
			| Self::Vector(_) => {}
		}
		Ok(bytes)
	}

	/// Constructs from runtime for `StorageValue`.
	///
	/// # Errors
	/// Rejects unsupported values, unresolved references or exceeded resource
	/// limits.
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
			Value::GraphRef(value) => context.graph_to_disk(&value).map(Self::GraphRef),
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
			Value::Geometry(value) => {
				value.validate()?;
				Ok(Self::Geometry(value))
			}
			Value::Vector(value) => Ok(Self::Vector(value)),
			Value::Atom(value) => StorageAtomRef::from_runtime(*value, context).map(Self::AtomRef),
			Value::Regex(_)
			| Value::Matcher(_)
			| Value::AtomOccurrence(_)
			| Value::Path(_)
			| Value::Graph(_)
			| Value::Closure(_)
			| Value::Error(_)
			| Value::Cursor(_) => Err(FerrosGrynnError::unsupported_storage_value(value.kind().to_string())),

			_ => Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported runtime variant at the workspace boundary.",
			)),
		}
	}

	/// Constructs from graph ref for `StorageValue`.
	///
	/// # Errors
	/// Rejects unsupported values, unresolved references or exceeded resource
	/// limits.
	pub fn from_graph_ref(value: &GraphRef, context: &EncodeContext) -> FerrosGrynnResult<Self> {
		context.graph_to_disk(value).map(Self::GraphRef)
	}

	/// Converts this value into runtime for `StorageValue`.
	///
	/// # Errors
	/// Rejects unsupported values, unresolved references or exceeded resource
	/// limits.
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
			Self::Geometry(value) => {
				value.validate()?;
				Ok(Value::Geometry(value))
			}
			Self::Vector(value) => Ok(Value::Vector(value)),
			Self::AtomRef(value) => value.into_runtime(context).map(Value::from),
			Self::GraphRef(value) => context.graph_from_disk(value).cloned().map(Value::from),
			Self::QualifiedAtomRef(_) => {
				Err(FerrosGrynnError::unsupported_storage_value("qualified atom reference"))
			}
		}
	}

	/// Converts this value into graph ref for `StorageValue`.
	///
	/// # Errors
	/// Rejects unsupported values, unresolved references or exceeded resource
	/// limits.
	pub fn into_graph_ref(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<GraphRef> {
		match self {
			Self::GraphRef(value) => context.graph_from_disk(value).cloned(),
			value => Err(FerrosGrynnError::conversion_error(value.type_name(), "GraphRef")),
		}
	}

	/// Returns the ref payload when this value has the matching variant.
	pub const fn as_ref(&self) -> StorageValueRef<'_> {
		StorageValueRef(self)
	}

	/// Returns type name for `StorageValue`.
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

/// Borrowed storage-value representation used without taking ownership of its
/// payload.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct StorageValueRef<'a>(
	/// Borrowed value represented without taking ownership of its payload.
	&'a StorageValue,
);

impl<'a> StorageValueRef<'a> {
	/// Constructs `StorageValueRef<'a>` from the supplied initial state.
	pub const fn new(value: &'a StorageValue) -> Self {
		Self(value)
	}
}

impl Deref for StorageValueRef<'_> {
	type Target = StorageValue;

	/// Borrows the underlying value represented by this `StorageValueRef<'_>`.
	fn deref(&self) -> &Self::Target {
		self.0
	}
}

impl StorageDateTime {
	/// Constructs from runtime for `StorageDateTime`.
	fn from_runtime(value: DateTime, context: &mut EncodeContext) -> FerrosGrynnResult<Self> {
		let zoned = value.into_inner();
		let timestamp = zoned.timestamp();
		let time_zone_id = context.intern_time_zone(&zoned)?;
		Ok(Self::from_timestamp(timestamp, time_zone_id))
	}

	/// Shares pre-epoch rounding without owning or cloning a runtime timestamp.
	fn from_timestamp(timestamp: Timestamp, time_zone_id: u32) -> Self {
		let nanoseconds = timestamp.as_nanosecond();
		let seconds = nanoseconds.div_euclid(1_000_000_000) as i64;
		let nanos = nanoseconds.rem_euclid(1_000_000_000) as u32;
		Self {
			seconds,
			nanos,
			time_zone_id,
		}
	}

	/// Converts this value into runtime for `StorageDateTime`.
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
	/// Bounds class-name and composite-part allocations before conversion.
	fn runtime_allocation_bytes(&self, context: &DecodeContext<'_>) -> FerrosGrynnResult<usize> {
		let class = context
			.classes
			.resolve(self.class_id)
			.ok_or_else(|| unresolved("class", self.class_id.to_string()))?;
		let mut bytes = 256usize;
		allocation_add(&mut bytes, class.len())?;
		match &self.part {
			StorageRecordIdPart::Array(values) => {
				for value in values {
					allocation_add(&mut bytes, record_value_allocation(value)?)?;
				}
			}
			StorageRecordIdPart::Object(values) => {
				allocation_add(
					&mut bytes,
					tree_allocation_bytes(values.len(), std::mem::size_of::<(Text, Value)>())?,
				)?;
				for (name, value) in values {
					allocation_add(&mut bytes, name.len())?;
					allocation_add(&mut bytes, record_value_allocation(value)?)?;
				}
			}
			StorageRecordIdPart::Int(_)
			| StorageRecordIdPart::String(_)
			| StorageRecordIdPart::Uuid(_)
			| StorageRecordIdPart::Ulid(_) => {}
		}
		Ok(bytes)
	}

	/// Constructs from runtime for `StorageRecordId`.
	pub(crate) fn from_runtime(
		value: RecordId,
		context: &EncodeContext,
	) -> FerrosGrynnResult<Self> {
		let (class, part) = value.into_parts();
		let class_id =
			context.class_id(&class).ok_or_else(|| unresolved("class", class.to_string()))?;
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

			_ => {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"Unsupported runtime variant at the workspace boundary.",
				));
			}
		};
		Ok(Self {
			class_id,
			part,
		})
	}

	/// Converts this value into runtime for `StorageRecordId`.
	fn into_runtime(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<RecordId> {
		let class = context
			.classes
			.resolve(self.class_id)
			.cloned()
			.ok_or_else(|| unresolved("class", self.class_id.to_string()))?;
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
		RecordId::new(class, part)
	}
}

/// Accumulates allocation reservations without saturating a hard cap.
fn allocation_add(bytes: &mut usize, additional: usize) -> FerrosGrynnResult<()> {
	*bytes = bytes
		.checked_add(additional)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("runtime conversion allocation"))?;
	Ok(())
}

/// Bounds composite RecordId conversion without materializing its runtime tree.
fn record_value_allocation(value: &StorageRecordValue) -> FerrosGrynnResult<usize> {
	let mut bytes = 256usize;
	match value {
		StorageRecordValue::Array(values) => {
			for value in values {
				allocation_add(&mut bytes, record_value_allocation(value)?)?;
			}
		}
		StorageRecordValue::Object(values) => {
			allocation_add(
				&mut bytes,
				tree_allocation_bytes(values.len(), std::mem::size_of::<(Text, Value)>())?,
			)?;
			for (name, value) in values {
				allocation_add(&mut bytes, name.len())?;
				allocation_add(&mut bytes, record_value_allocation(value)?)?;
			}
		}
		StorageRecordValue::Int(_)
		| StorageRecordValue::String(_)
		| StorageRecordValue::Uuid(_)
		| StorageRecordValue::Ulid(_) => {}
	}
	Ok(bytes)
}

impl StorageRange {
	/// Constructs from runtime for `StorageRange`.
	fn from_runtime(value: Range, context: &mut EncodeContext) -> FerrosGrynnResult<Self> {
		let (start, end) = value.into_bounds();
		Ok(Self {
			start: StorageBound::from_runtime(start, context)?,
			end: StorageBound::from_runtime(end, context)?,
		})
	}

	/// Converts this value into runtime for `StorageRange`.
	fn into_runtime(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<Range> {
		Range::new_with_diagnostic_admission(
			self.start.into_runtime(context)?,
			self.end.into_runtime(context)?,
			|_, _| Err(FerrosGrynnError::invalid_storage_file("Stored range bounds are invalid")),
		)
	}
}

impl StorageBound {
	/// Constructs from runtime for `StorageBound`.
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

	/// Converts this value into runtime for `StorageBound`.
	fn into_runtime(self, context: &DecodeContext<'_>) -> FerrosGrynnResult<Bound<Value>> {
		match self {
			Self::Unbounded => Ok(Bound::Unbounded),
			Self::Included(value) => value.into_runtime(context).map(Bound::Included),
			Self::Excluded(value) => value.into_runtime(context).map(Bound::Excluded),
		}
	}
}
