use super::{
	CanonicalCodec,
	DURATION_SIGN,
	DURATION_UNIT_MASK,
	RANGE_LOWER_INCLUSIVE,
	RANGE_LOWER_UNBOUNDED,
	RANGE_UPPER_INCLUSIVE,
	RANGE_UPPER_UNBOUNDED,
	canonical_f32,
	invalid,
};
use crate::{
	gql_status::FerrosGrynnResult,
	values::{
		Geometry,
		Position,
		Text,
		Vector,
		storage::{
			StorageBound,
			StorageDuration,
			StorageRange,
			StorageRecordId,
			StorageRecordIdPart,
			StorageRecordValue,
			StorageValue,
			TypeTag,
			canonical_f64,
		},
	},
};

/// Returns canonicalize set for this module.
pub(in crate::values::storage) fn canonicalize_set(
	values: &mut Vec<StorageValue>,
) -> FerrosGrynnResult<()> {
	let mut encoded = Vec::with_capacity(values.len());
	for value in values.drain(..) {
		let bytes = CanonicalCodec::encode(value.as_ref())?;
		encoded.push((bytes, value));
	}
	finish_set(values, encoded)
}

/// Shares ordering and duplicate validation across growing and reserved
/// writers.
pub(in crate::values::storage) fn finish_set(
	values: &mut Vec<StorageValue>,
	mut encoded: Vec<(Vec<u8>, StorageValue)>,
) -> FerrosGrynnResult<()> {
	encoded.sort_unstable_by(|left, right| left.0.cmp(&right.0));
	if encoded.windows(2).any(|pair| pair[0].0 == pair[1].0) {
		return Err(invalid(0, "set contains duplicate canonical values"));
	}
	values.extend(encoded.into_iter().map(|(_bytes, value)| value));
	Ok(())
}

/// Encodes value for this module.
pub(super) fn encode_value(
	output: &mut Vec<u8>,
	value: &StorageValue,
	reserve_children: bool,
) -> FerrosGrynnResult<()> {
	match value {
		StorageValue::None => output.push(TypeTag::None.byte()),
		StorageValue::Null => output.push(TypeTag::Null.byte()),
		StorageValue::Bool(false) => output.push(TypeTag::False.byte()),
		StorageValue::Bool(true) => output.push(TypeTag::True.byte()),
		StorageValue::Int(value) => {
			output.push(TypeTag::Int.byte());
			write_uleb(output, zigzag_encode(*value));
		}
		StorageValue::Float(bits) => {
			if canonical_f64(f64::from_bits(*bits)) != *bits {
				return Err(invalid(output.len(), "float bits are not canonical"));
			}
			output.push(TypeTag::Float.byte());
			output.extend_from_slice(&bits.to_le_bytes());
		}
		StorageValue::Decimal(value) => {
			output.push(TypeTag::Decimal.byte());
			output.extend_from_slice(&value.mantissa.to_le_bytes());
			output.push(value.scale);
		}
		StorageValue::String(value) => {
			output.push(TypeTag::String.byte());
			write_bytes(output, value.as_bytes());
		}
		StorageValue::Bytes(value) => {
			output.push(TypeTag::Bytes.byte());
			write_bytes(output, value.as_slice());
		}
		StorageValue::DateTime(value) => {
			if value.nanos >= 1_000_000_000 {
				return Err(invalid(output.len(), "datetime nanoseconds exceed one second"));
			}
			output.push(TypeTag::DateTime.byte());
			output.extend_from_slice(&value.seconds.to_le_bytes());
			output.extend_from_slice(&value.nanos.to_le_bytes());
			output.extend_from_slice(&value.time_zone_id.to_le_bytes());
		}
		StorageValue::Duration(value) => encode_duration(output, value)?,
		StorageValue::Uuid(value) => {
			output.push(TypeTag::Uuid.byte());
			output.extend_from_slice(value.as_inner().as_bytes());
		}
		StorageValue::Ulid(value) => {
			output.push(TypeTag::Ulid.byte());
			output.extend_from_slice(&u128::from(value.into_inner()).to_be_bytes());
		}
		StorageValue::Record(value) => encode_record_id(output, value)?,
		StorageValue::Array(values) => {
			output.push(TypeTag::Array.byte());
			write_count(output, values.len())?;
			for value in values {
				encode_value(output, value, reserve_children)?;
			}
		}
		StorageValue::Set(values) => {
			output.push(TypeTag::Set.byte());
			write_count(output, values.len())?;
			let mut previous: Option<Vec<u8>> = None;
			for value in values {
				let encoded = if reserve_children {
					// Parent admission already includes these nested child
					// buffers.
					CanonicalCodec::encode_bounded(value.as_ref(), usize::MAX)?
				} else {
					CanonicalCodec::encode(value.as_ref())?
				};
				if previous
					.as_ref()
					.is_some_and(|previous| previous.as_slice() >= encoded.as_slice())
				{
					return Err(invalid(
						output.len(),
						"set values are not in strict canonical order",
					));
				}
				output.extend_from_slice(&encoded);
				previous = Some(encoded);
			}
		}
		StorageValue::Object(values) => {
			output.push(TypeTag::Object.byte());
			write_count(output, values.len())?;
			let mut previous = None;
			for (key, value) in values {
				if previous.is_some_and(|previous| previous >= *key) {
					return Err(invalid(
						output.len(),
						"object keys are not in strict key_id order",
					));
				}
				output.extend_from_slice(&key.get().to_le_bytes());
				encode_value(output, value, reserve_children)?;
				previous = Some(*key);
			}
		}
		StorageValue::Range(value) => encode_range(output, value, reserve_children)?,
		StorageValue::Geometry(value) => {
			value.validate()?;
			encode_geometry(output, value)?;
		}
		StorageValue::Vector(value) => encode_vector(output, value)?,
		StorageValue::AtomRef(value) => {
			output.push(TypeTag::AtomRef.byte());
			output.push(match value.kind {
				crate::values::AtomKind::Node => 0,
				crate::values::AtomKind::Edge => 1,

				_ => {
					return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"Unsupported runtime variant at the workspace boundary.",
					));
				}
			});
			encode_record_id(output, &value.record_id)?;
		}
		StorageValue::GraphRef(value) => {
			output.push(TypeTag::GraphRef.byte());
			output.extend_from_slice(&value.raw().to_le_bytes());
		}
		StorageValue::QualifiedAtomRef(value) => {
			output.push(TypeTag::QualifiedAtomRef.byte());
			output.extend_from_slice(&value.graph_slot.to_le_bytes());
			output.extend_from_slice(&value.local_ref.raw().to_le_bytes());
		}
	}
	Ok(())
}

/// Encodes duration for this module.
fn encode_duration(output: &mut Vec<u8>, value: &StorageDuration) -> FerrosGrynnResult<()> {
	let mut header = if value.negative {
		DURATION_SIGN
	} else {
		0
	};
	for (index, unit) in value.units.iter().enumerate() {
		if *unit != 0 {
			header |= 1 << (14 - index);
		}
	}
	if value.negative && header & DURATION_UNIT_MASK == 0 {
		return Err(invalid(output.len(), "negative zero duration is not canonical"));
	}
	value.to_span().map_err(|cause| {
		invalid(output.len(), "duration components are outside jiff::Span limits").with_cause(cause)
	})?;
	output.push(TypeTag::Duration.byte());
	output.extend_from_slice(&header.to_le_bytes());
	for unit in value.units.iter().filter(|unit| **unit != 0) {
		write_uleb(output, *unit);
	}
	Ok(())
}

/// Encodes record id for this module.
fn encode_record_id(output: &mut Vec<u8>, value: &StorageRecordId) -> FerrosGrynnResult<()> {
	output.push(TypeTag::RecordId.byte());
	output.extend_from_slice(&value.class_id.to_le_bytes());
	match &value.part {
		StorageRecordIdPart::Int(value) => {
			output.push(0x00);
			write_uleb(output, zigzag_encode(*value));
		}
		StorageRecordIdPart::String(value) => {
			output.push(0x01);
			write_bytes(output, value.as_bytes());
		}
		StorageRecordIdPart::Uuid(value) => {
			output.push(0x02);
			output.extend_from_slice(value.as_inner().as_bytes());
		}
		StorageRecordIdPart::Ulid(value) => {
			output.push(0x03);
			output.extend_from_slice(&u128::from(value.into_inner()).to_be_bytes());
		}
		StorageRecordIdPart::Array(values) => {
			output.push(0x04);
			write_count(output, values.len())?;
			for value in values {
				encode_record_value(output, value)?;
			}
		}
		StorageRecordIdPart::Object(values) => {
			output.push(0x05);
			encode_record_object(output, values)?;
		}
	}
	Ok(())
}

/// Encodes record value for this module.
fn encode_record_value(output: &mut Vec<u8>, value: &StorageRecordValue) -> FerrosGrynnResult<()> {
	match value {
		StorageRecordValue::Int(value) => {
			output.push(TypeTag::Int.byte());
			write_uleb(output, zigzag_encode(*value));
		}
		StorageRecordValue::String(value) => {
			output.push(TypeTag::String.byte());
			write_bytes(output, value.as_bytes());
		}
		StorageRecordValue::Uuid(value) => {
			output.push(TypeTag::Uuid.byte());
			output.extend_from_slice(value.as_inner().as_bytes());
		}
		StorageRecordValue::Ulid(value) => {
			output.push(TypeTag::Ulid.byte());
			output.extend_from_slice(&u128::from(value.into_inner()).to_be_bytes());
		}
		StorageRecordValue::Array(values) => {
			output.push(TypeTag::Array.byte());
			write_count(output, values.len())?;
			for value in values {
				encode_record_value(output, value)?;
			}
		}
		StorageRecordValue::Object(values) => {
			output.push(TypeTag::Object.byte());
			encode_record_object(output, values)?;
		}
	}
	Ok(())
}

/// Encodes record object for this module.
fn encode_record_object(
	output: &mut Vec<u8>,
	values: &[(Text, StorageRecordValue)],
) -> FerrosGrynnResult<()> {
	write_count(output, values.len())?;
	let mut previous: Option<&[u8]> = None;
	for (key, value) in values {
		let bytes = key.as_bytes();
		if previous.is_some_and(|previous| previous >= bytes) {
			return Err(invalid(
				output.len(),
				"RecordId object keys are not in strict UTF-8 order",
			));
		}
		write_bytes(output, bytes);
		encode_record_value(output, value)?;
		previous = Some(bytes);
	}
	Ok(())
}

/// Encodes range for this module.
fn encode_range(
	output: &mut Vec<u8>,
	value: &StorageRange,
	reserve_children: bool,
) -> FerrosGrynnResult<()> {
	output.push(TypeTag::Range.byte());
	let mut flags = 0;
	flags |= match value.start {
		StorageBound::Unbounded => RANGE_LOWER_UNBOUNDED,
		StorageBound::Included(_) => RANGE_LOWER_INCLUSIVE,
		StorageBound::Excluded(_) => 0,
	};
	flags |= match value.end {
		StorageBound::Unbounded => RANGE_UPPER_UNBOUNDED,
		StorageBound::Included(_) => RANGE_UPPER_INCLUSIVE,
		StorageBound::Excluded(_) => 0,
	};
	output.push(flags);
	if let StorageBound::Included(value) | StorageBound::Excluded(value) = &value.start {
		encode_value(output, value, reserve_children)?;
	}
	if let StorageBound::Included(value) | StorageBound::Excluded(value) = &value.end {
		encode_value(output, value, reserve_children)?;
	}
	Ok(())
}

/// Encodes geometry for this module.
fn encode_geometry(output: &mut Vec<u8>, value: &Geometry) -> FerrosGrynnResult<()> {
	match value {
		Geometry::Point(value) => {
			output.push(TypeTag::Point.byte());
			encode_position(output, value);
		}
		Geometry::LineString(value) => {
			output.push(TypeTag::LineString.byte());
			encode_positions(output, value)?;
		}
		Geometry::Polygon(value) => {
			output.push(TypeTag::Polygon.byte());
			encode_polygon_body(output, value)?;
		}
		Geometry::MultiPoint(value) => {
			output.push(TypeTag::MultiPoint.byte());
			encode_positions(output, value)?;
		}
		Geometry::MultiLineString(value) => {
			output.push(TypeTag::MultiLineString.byte());
			write_u32_count(output, value.len())?;
			for line in value {
				encode_positions(output, line)?;
			}
		}
		Geometry::MultiPolygon(value) => {
			output.push(TypeTag::MultiPolygon.byte());
			write_u32_count(output, value.len())?;
			for polygon in value {
				encode_polygon_body(output, polygon)?;
			}
		}
		Geometry::Collection(value) => {
			output.push(TypeTag::GeometryCollection.byte());
			write_u32_count(output, value.len())?;
			for geometry in value {
				encode_geometry(output, geometry)?;
			}
		}

		_ => {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported runtime variant at the workspace boundary.",
			));
		}
	}
	Ok(())
}

/// Encodes polygon body for this module.
fn encode_polygon_body(output: &mut Vec<u8>, rings: &[Vec<Position>]) -> FerrosGrynnResult<()> {
	write_u32_count(output, rings.len())?;
	for ring in rings {
		encode_positions(output, ring)?;
	}
	Ok(())
}

/// Encodes positions for this module.
fn encode_positions(output: &mut Vec<u8>, values: &[Position]) -> FerrosGrynnResult<()> {
	write_u32_count(output, values.len())?;
	for value in values {
		encode_position(output, value);
	}
	Ok(())
}

/// Encodes position for this module.
fn encode_position(output: &mut Vec<u8>, value: &Position) {
	for coordinate in value.as_slice() {
		output.extend_from_slice(&canonical_f64(*coordinate).to_le_bytes());
	}
}

/// Encodes vector for this module.
fn encode_vector(output: &mut Vec<u8>, value: &Vector) -> FerrosGrynnResult<()> {
	let dimension = u16::try_from(value.len())
		.map_err(|_err| invalid(output.len(), "vector dimension exceeds u16"))?;
	match value {
		Vector::F64(values) => {
			output.push(TypeTag::VectorF64.byte());
			output.extend_from_slice(&dimension.to_le_bytes());
			for value in values {
				output.extend_from_slice(&canonical_f64(*value).to_le_bytes());
			}
		}
		Vector::F32(values) => {
			output.push(TypeTag::VectorF32.byte());
			output.extend_from_slice(&dimension.to_le_bytes());
			for value in values {
				output.extend_from_slice(&canonical_f32(*value).to_le_bytes());
			}
		}
		Vector::F16(values) => {
			output.push(TypeTag::VectorF16.byte());
			output.extend_from_slice(&dimension.to_le_bytes());
			for value in values {
				output.extend_from_slice(&canonical_f16(*value).to_le_bytes());
			}
		}
		Vector::I64(values) => encode_fixed_vector(output, TypeTag::VectorI64, dimension, values),
		Vector::I32(values) => encode_fixed_vector(output, TypeTag::VectorI32, dimension, values),
		Vector::I16(values) => encode_fixed_vector(output, TypeTag::VectorI16, dimension, values),
		Vector::I8(values) => encode_fixed_vector(output, TypeTag::VectorI8, dimension, values),
		Vector::U8(values) => encode_fixed_vector(output, TypeTag::VectorU8, dimension, values),

		_ => {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported runtime variant at the workspace boundary.",
			));
		}
	}
	Ok(())
}

/// Returns canonical f16 for this module.
fn canonical_f16(value: half::f16) -> u16 {
	if value.is_nan() {
		half::f16::NAN.to_bits()
	} else if value == half::f16::ZERO {
		half::f16::ZERO.to_bits()
	} else {
		value.to_bits()
	}
}

/// Scalar value that can append its canonical little-endian bytes.
trait LittleEndianElement {
	/// Appends le for `LittleEndianElement`.
	fn append_le(&self, output: &mut Vec<u8>);
}

/// Implements canonical little-endian byte output for primitive numeric
/// elements.
macro_rules! impl_little_endian_element {
	($($type:ty),* $(,)?) => {
		$(
			impl LittleEndianElement for $type {
				/// Appends the scalar value in little-endian byte order.
				fn append_le(&self, output: &mut Vec<u8>) {
					output.extend_from_slice(&self.to_le_bytes());
				}
			}
		)*
	};
}

impl_little_endian_element!(i8, i16, i32, i64, u8);

/// Encodes fixed vector for this module.
fn encode_fixed_vector<T: LittleEndianElement>(
	output: &mut Vec<u8>,
	tag: TypeTag,
	dimension: u16,
	values: &[T],
) {
	output.push(tag.byte());
	output.extend_from_slice(&dimension.to_le_bytes());
	for value in values {
		value.append_le(output);
	}
}

/// Writes count for this module.
fn write_count(output: &mut Vec<u8>, count: usize) -> FerrosGrynnResult<()> {
	let count = u64::try_from(count).map_err(|_err| invalid(output.len(), "length exceeds u64"))?;
	write_uleb(output, count);
	Ok(())
}

/// Writes u32 count for this module.
fn write_u32_count(output: &mut Vec<u8>, count: usize) -> FerrosGrynnResult<()> {
	let count =
		u32::try_from(count).map_err(|_err| invalid(output.len(), "geometry count exceeds u32"))?;
	output.extend_from_slice(&count.to_le_bytes());
	Ok(())
}

/// Writes bytes for this module.
fn write_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
	write_uleb(output, bytes.len() as u64);
	output.extend_from_slice(bytes);
}

/// Writes uleb for this module.
fn write_uleb(output: &mut Vec<u8>, mut value: u64) {
	loop {
		let mut byte = (value & 0x7F) as u8;
		value >>= 7;
		if value != 0 {
			byte |= 0x80;
		}
		output.push(byte);
		if value == 0 {
			break;
		}
	}
}

/// Returns zigzag encode for this module.
pub(super) const fn zigzag_encode(value: i64) -> u64 {
	((value as u64) << 1) ^ ((value >> 63) as u64)
}

/// Returns zigzag decode for this module.
pub(super) const fn zigzag_decode(value: u64) -> i64 {
	((value >> 1) as i64) ^ -((value & 1) as i64)
}
