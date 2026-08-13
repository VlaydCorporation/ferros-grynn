#![allow(
	clippy::missing_docs_in_private_items,
	reason = "private codec primitives directly implement the documented format table and are \
	          tested through CanonicalCodec"
)]

//! Canonical format-version-1 codec and checked archive envelope.

use std::{mem::size_of, str};

use rkyv::util::AlignedVec;

use super::{
	DiskAtomRef,
	DiskGraphRef,
	QualifiedAtomRef,
	StorageBound,
	StorageDateTime,
	StorageDecimal,
	StorageDuration,
	StorageRange,
	StorageRecordId,
	StorageRecordIdPart,
	StorageRecordValue,
	StorageValue,
	StorageValueRef,
	TypeTag,
	canonical_f64,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Bytes, Geometry, Position, PropertyKeyId, Text, Ulid, Uuid, Vector},
};

const RANGE_LOWER_INCLUSIVE: u8 = 1 << 0;
const RANGE_UPPER_INCLUSIVE: u8 = 1 << 1;
const RANGE_LOWER_UNBOUNDED: u8 = 1 << 2;
const RANGE_UPPER_UNBOUNDED: u8 = 1 << 3;
const RANGE_ALLOWED_FLAGS: u8 = 0x0F;
const DURATION_SIGN: u16 = 1 << 15;
const DURATION_UNIT_MASK: u16 = 0x7FE0;
const ARCHIVE_MAGIC: [u8; 4] = *b"FGAR";
const ARCHIVE_FORMAT_VERSION: u16 = 1;
const ARCHIVE_HEADER_LEN: usize = 32;
const ARCHIVE_RESERVED_LEN: usize = ARCHIVE_HEADER_LEN - (4 + 2 + 4 + 8 + 4);

/// Resource bounds applied before allocating from untrusted encoded lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
	pub max_depth: usize,
	pub max_collection_len: usize,
	pub max_string_bytes: usize,
	pub max_blob_bytes: usize,
	pub max_total_allocation: usize,
}

impl DecodeLimits {
	pub const fn new(
		max_depth: usize,
		max_collection_len: usize,
		max_string_bytes: usize,
		max_blob_bytes: usize,
		max_total_allocation: usize,
	) -> Self {
		Self {
			max_depth,
			max_collection_len,
			max_string_bytes,
			max_blob_bytes,
			max_total_allocation,
		}
	}
}

impl Default for DecodeLimits {
	fn default() -> Self {
		Self::new(64, 1_000_000, 16 * 1024 * 1024, 64 * 1024 * 1024, 128 * 1024 * 1024)
	}
}

/// Canonical value codec defined by `disk_storage_spec.md` section 5.
pub struct CanonicalCodec;

impl CanonicalCodec {
	pub fn encode(value: StorageValueRef<'_>) -> FerrosGrynnResult<Vec<u8>> {
		let mut output = Vec::new();
		encode_value(&mut output, &value)?;
		Ok(output)
	}

	pub fn decode(bytes: &[u8], limits: DecodeLimits) -> FerrosGrynnResult<StorageValue> {
		let mut decoder = Decoder::new(bytes, limits);
		let value = decoder.decode_value(0)?;
		if decoder.position != bytes.len() {
			return Err(invalid(decoder.position, "trailing bytes after StorageValue"));
		}
		Ok(value)
	}
}

pub(super) fn canonicalize_set(values: &mut Vec<StorageValue>) -> FerrosGrynnResult<()> {
	let mut encoded = Vec::with_capacity(values.len());
	for value in values.drain(..) {
		let bytes = CanonicalCodec::encode(value.as_ref())?;
		encoded.push((bytes, value));
	}
	encoded.sort_unstable_by(|left, right| left.0.cmp(&right.0));
	if encoded.windows(2).any(|pair| pair[0].0 == pair[1].0) {
		return Err(invalid(0, "set contains duplicate canonical values"));
	}
	values.extend(encoded.into_iter().map(|(_bytes, value)| value));
	Ok(())
}

fn encode_value(output: &mut Vec<u8>, value: &StorageValue) -> FerrosGrynnResult<()> {
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
				encode_value(output, value)?;
			}
		}
		StorageValue::Set(values) => {
			output.push(TypeTag::Set.byte());
			write_count(output, values.len())?;
			let mut previous: Option<Vec<u8>> = None;
			for value in values {
				let encoded = CanonicalCodec::encode(value.as_ref())?;
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
				encode_value(output, value)?;
				previous = Some(*key);
			}
		}
		StorageValue::Range(value) => encode_range(output, value)?,
		StorageValue::Geometry(value) => encode_geometry(output, value)?,
		StorageValue::Vector(value) => encode_vector(output, value)?,
		StorageValue::AtomRef(value) => {
			output.push(TypeTag::AtomRef.byte());
			output.extend_from_slice(&value.raw().to_le_bytes());
		}
		StorageValue::GraphRef(value) => {
			output.push(TypeTag::GraphRef.byte());
			output.extend_from_slice(&value.slot.to_le_bytes());
			output.extend_from_slice(&value.generation.to_le_bytes());
		}
		StorageValue::QualifiedAtomRef(value) => {
			output.push(TypeTag::QualifiedAtomRef.byte());
			output.extend_from_slice(&value.graph_slot.to_le_bytes());
			output.extend_from_slice(&value.local_ref.raw().to_le_bytes());
		}
	}
	Ok(())
}

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

fn encode_record_id(output: &mut Vec<u8>, value: &StorageRecordId) -> FerrosGrynnResult<()> {
	output.push(TypeTag::RecordId.byte());
	output.extend_from_slice(&value.table_id.to_le_bytes());
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

fn encode_range(output: &mut Vec<u8>, value: &StorageRange) -> FerrosGrynnResult<()> {
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
		encode_value(output, value)?;
	}
	if let StorageBound::Included(value) | StorageBound::Excluded(value) = &value.end {
		encode_value(output, value)?;
	}
	Ok(())
}

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
	}
	Ok(())
}

fn encode_polygon_body(output: &mut Vec<u8>, rings: &[Vec<Position>]) -> FerrosGrynnResult<()> {
	write_u32_count(output, rings.len())?;
	for ring in rings {
		encode_positions(output, ring)?;
	}
	Ok(())
}

fn encode_positions(output: &mut Vec<u8>, values: &[Position]) -> FerrosGrynnResult<()> {
	write_u32_count(output, values.len())?;
	for value in values {
		encode_position(output, value);
	}
	Ok(())
}

fn encode_position(output: &mut Vec<u8>, value: &Position) {
	for coordinate in value.as_slice() {
		output.extend_from_slice(&canonical_f64(*coordinate).to_le_bytes());
	}
}

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
		Vector::I64(values) => encode_fixed_vector(output, TypeTag::VectorI64, dimension, values),
		Vector::I32(values) => encode_fixed_vector(output, TypeTag::VectorI32, dimension, values),
		Vector::I16(values) => encode_fixed_vector(output, TypeTag::VectorI16, dimension, values),
	}
	Ok(())
}

trait LittleEndianElement {
	fn append_le(&self, output: &mut Vec<u8>);
}

macro_rules! impl_little_endian_element {
	($($type:ty),* $(,)?) => {
		$(
			impl LittleEndianElement for $type {
				fn append_le(&self, output: &mut Vec<u8>) {
					output.extend_from_slice(&self.to_le_bytes());
				}
			}
		)*
	};
}

impl_little_endian_element!(i16, i32, i64);

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

fn write_count(output: &mut Vec<u8>, count: usize) -> FerrosGrynnResult<()> {
	let count = u64::try_from(count).map_err(|_err| invalid(output.len(), "length exceeds u64"))?;
	write_uleb(output, count);
	Ok(())
}

fn write_u32_count(output: &mut Vec<u8>, count: usize) -> FerrosGrynnResult<()> {
	let count =
		u32::try_from(count).map_err(|_err| invalid(output.len(), "geometry count exceeds u32"))?;
	output.extend_from_slice(&count.to_le_bytes());
	Ok(())
}

fn write_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
	write_uleb(output, bytes.len() as u64);
	output.extend_from_slice(bytes);
}

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

const fn zigzag_encode(value: i64) -> u64 {
	((value as u64) << 1) ^ ((value >> 63) as u64)
}

const fn zigzag_decode(value: u64) -> i64 {
	((value >> 1) as i64) ^ -((value & 1) as i64)
}

struct Decoder<'a> {
	bytes: &'a [u8],
	position: usize,
	limits: DecodeLimits,
	allocated: usize,
}

impl<'a> Decoder<'a> {
	const fn new(bytes: &'a [u8], limits: DecodeLimits) -> Self {
		Self {
			bytes,
			position: 0,
			limits,
			allocated: 0,
		}
	}

	fn decode_value(&mut self, depth: usize) -> FerrosGrynnResult<StorageValue> {
		self.check_depth(depth)?;
		let offset = self.position;
		let tag = self.read_u8()?;
		match tag {
			0x00 => Ok(StorageValue::None),
			0x01 => Ok(StorageValue::Null),
			0x02 => Ok(StorageValue::Bool(false)),
			0x03 => Ok(StorageValue::Bool(true)),
			0x04 => self.read_uleb().map(zigzag_decode).map(StorageValue::Int),
			0x05 => {
				let bits = self.read_u64()?;
				if canonical_f64(f64::from_bits(bits)) != bits {
					return Err(invalid(offset, "float bits are not canonical"));
				}
				Ok(StorageValue::Float(bits))
			}
			0x06 => Ok(StorageValue::Decimal(StorageDecimal {
				mantissa: self.read_i128()?,
				scale: self.read_u8()?,
			})),
			0x07 => self.read_text().map(StorageValue::String),
			0x08 => self.read_blob().map(Bytes::from).map(StorageValue::Bytes),
			0x09 => self.decode_datetime(offset).map(StorageValue::DateTime),
			0x0A => self.decode_duration(offset).map(StorageValue::Duration),
			0x0B => self.read_uuid().map(StorageValue::Uuid),
			0x0C => self.read_ulid().map(StorageValue::Ulid),
			0x10 => self.decode_record_id(depth + 1).map(StorageValue::Record),
			0x11 => self.decode_values(depth + 1).map(StorageValue::Array),
			0x12 => self.decode_set(depth + 1).map(StorageValue::Set),
			0x13 => self.decode_object(depth + 1).map(StorageValue::Object),
			0x15 => self.decode_range(depth + 1, offset).map(StorageValue::Range),
			0x21..=0x27 => self.decode_geometry(tag, depth + 1).map(StorageValue::Geometry),
			0x31..=0x35 => self.decode_vector(tag).map(StorageValue::Vector),
			0x36 => {
				let raw = self.read_u64()?;
				DiskAtomRef::from_raw(raw).map(StorageValue::AtomRef).map_err(|cause| {
					invalid(offset, "DiskAtomRef contains reserved bits").with_cause(cause)
				})
			}
			0x37 => Ok(StorageValue::GraphRef(DiskGraphRef {
				slot: self.read_u32()?,
				generation: self.read_u32()?,
			})),
			0x38 => {
				let graph_slot = self.read_u32()?;
				let raw = self.read_u64()?;
				let local_ref = DiskAtomRef::from_raw(raw).map_err(|cause| {
					invalid(offset, "qualified DiskAtomRef contains reserved bits")
						.with_cause(cause)
				})?;
				Ok(StorageValue::QualifiedAtomRef(QualifiedAtomRef {
					graph_slot,
					local_ref,
				}))
			}
			unknown => {
				Err(FerrosGrynnError::unknown_storage_type_tag(u64::from(unknown), offset as u64))
			}
		}
	}

	fn decode_datetime(&mut self, offset: usize) -> FerrosGrynnResult<StorageDateTime> {
		let seconds = self.read_i64()?;
		let nanos = self.read_u32()?;
		let time_zone_id = self.read_u32()?;
		if nanos >= 1_000_000_000 {
			return Err(invalid(offset, "datetime nanoseconds exceed one second"));
		}
		if time_zone_id > 1_172_799 {
			return Err(invalid(offset, "datetime fixed time-zone identifier is out of range"));
		}
		Ok(StorageDateTime {
			seconds,
			nanos,
			time_zone_id,
		})
	}

	fn decode_duration(&mut self, offset: usize) -> FerrosGrynnResult<StorageDuration> {
		let header = self.read_u16()?;
		if header & 0x1F != 0 {
			return Err(invalid(offset, "duration reserved header bits are non-zero"));
		}
		let mut units = [0_u64; 10];
		for (index, unit) in units.iter_mut().enumerate() {
			if header & (1 << (14 - index)) != 0 {
				*unit = self.read_uleb()?;
				if *unit == 0 {
					return Err(invalid(offset, "duration stores a zero-valued present unit"));
				}
			}
		}
		let value = StorageDuration {
			negative: header & DURATION_SIGN != 0,
			units,
		};
		if value.negative && header & DURATION_UNIT_MASK == 0 {
			return Err(invalid(offset, "negative zero duration is not canonical"));
		}
		value.to_span().map_err(|cause| {
			invalid(offset, "duration components are outside jiff::Span limits").with_cause(cause)
		})?;
		Ok(value)
	}

	fn decode_record_id(&mut self, depth: usize) -> FerrosGrynnResult<StorageRecordId> {
		self.check_depth(depth)?;
		let table_id = self.read_u32()?;
		let kind_offset = self.position;
		let part = match self.read_u8()? {
			0x00 => StorageRecordIdPart::Int(zigzag_decode(self.read_uleb()?)),
			0x01 => StorageRecordIdPart::String(self.read_text()?),
			0x02 => StorageRecordIdPart::Uuid(self.read_uuid()?),
			0x03 => StorageRecordIdPart::Ulid(self.read_ulid()?),
			0x04 => StorageRecordIdPart::Array(self.decode_record_values(depth + 1)?),
			0x05 => StorageRecordIdPart::Object(self.decode_record_object(depth + 1)?),
			_ => return Err(invalid(kind_offset, "unknown RecordId part kind")),
		};
		Ok(StorageRecordId {
			table_id,
			part,
		})
	}

	fn decode_record_values(&mut self, depth: usize) -> FerrosGrynnResult<Vec<StorageRecordValue>> {
		let count = self.read_collection_len()?;
		let mut values = self.allocate_vec(count)?;
		for _ in 0..count {
			values.push(self.decode_record_value(depth)?);
		}
		Ok(values)
	}

	fn decode_record_value(&mut self, depth: usize) -> FerrosGrynnResult<StorageRecordValue> {
		self.check_depth(depth)?;
		let offset = self.position;
		match self.read_u8()? {
			0x04 => Ok(StorageRecordValue::Int(zigzag_decode(self.read_uleb()?))),
			0x07 => self.read_text().map(StorageRecordValue::String),
			0x0B => self.read_uuid().map(StorageRecordValue::Uuid),
			0x0C => self.read_ulid().map(StorageRecordValue::Ulid),
			0x11 => self.decode_record_values(depth + 1).map(StorageRecordValue::Array),
			0x13 => self.decode_record_object(depth + 1).map(StorageRecordValue::Object),
			_ => Err(invalid(offset, "StorageValue type is not valid inside RecordId")),
		}
	}

	fn decode_record_object(
		&mut self,
		depth: usize,
	) -> FerrosGrynnResult<Vec<(Text, StorageRecordValue)>> {
		self.check_depth(depth)?;
		let count = self.read_collection_len()?;
		let mut values: Vec<(Text, StorageRecordValue)> = self.allocate_vec(count)?;
		for _ in 0..count {
			let key_offset = self.position;
			let key = self.read_text()?;
			if values.last().is_some_and(|(previous, _value)| previous.as_bytes() >= key.as_bytes())
			{
				return Err(invalid(
					key_offset,
					"RecordId object keys are duplicate or out of UTF-8 order",
				));
			}
			let value = self.decode_record_value(depth + 1)?;
			values.push((key, value));
		}
		Ok(values)
	}

	fn decode_values(&mut self, depth: usize) -> FerrosGrynnResult<Vec<StorageValue>> {
		let count = self.read_collection_len()?;
		let mut values = self.allocate_vec(count)?;
		for _ in 0..count {
			values.push(self.decode_value(depth)?);
		}
		Ok(values)
	}

	fn decode_set(&mut self, depth: usize) -> FerrosGrynnResult<Vec<StorageValue>> {
		let count = self.read_collection_len()?;
		let mut values = self.allocate_vec(count)?;
		let mut previous: Option<&[u8]> = None;
		for _ in 0..count {
			let start = self.position;
			let value = self.decode_value(depth)?;
			let encoded = self
				.bytes
				.get(start..self.position)
				.ok_or_else(|| invalid(start, "set element slice is outside input"))?;
			if previous.is_some_and(|previous| previous >= encoded) {
				return Err(invalid(start, "set values are duplicate or out of canonical order"));
			}
			previous = Some(encoded);
			values.push(value);
		}
		Ok(values)
	}

	fn decode_object(
		&mut self,
		depth: usize,
	) -> FerrosGrynnResult<Vec<(PropertyKeyId, StorageValue)>> {
		let count = self.read_collection_len()?;
		let mut values = self.allocate_vec(count)?;
		let mut previous = None;
		for _ in 0..count {
			let key_offset = self.position;
			let key = PropertyKeyId::new(self.read_u32()?);
			if previous.is_some_and(|previous| previous >= key) {
				return Err(invalid(
					key_offset,
					"object keys are duplicate or out of key_id order",
				));
			}
			let value = self.decode_value(depth)?;
			previous = Some(key);
			values.push((key, value));
		}
		Ok(values)
	}

	fn decode_range(&mut self, depth: usize, offset: usize) -> FerrosGrynnResult<StorageRange> {
		let flags = self.read_u8()?;
		if flags & !RANGE_ALLOWED_FLAGS != 0 {
			return Err(invalid(offset, "range reserved flags are non-zero"));
		}
		if flags & RANGE_LOWER_UNBOUNDED != 0 && flags & RANGE_LOWER_INCLUSIVE != 0
			|| flags & RANGE_UPPER_UNBOUNDED != 0 && flags & RANGE_UPPER_INCLUSIVE != 0
		{
			return Err(invalid(offset, "unbounded range endpoint cannot be inclusive"));
		}
		let start = if flags & RANGE_LOWER_UNBOUNDED != 0 {
			StorageBound::Unbounded
		} else {
			let value = self.decode_boxed_value(depth)?;
			if flags & RANGE_LOWER_INCLUSIVE != 0 {
				StorageBound::Included(value)
			} else {
				StorageBound::Excluded(value)
			}
		};
		let end = if flags & RANGE_UPPER_UNBOUNDED != 0 {
			StorageBound::Unbounded
		} else {
			let value = self.decode_boxed_value(depth)?;
			if flags & RANGE_UPPER_INCLUSIVE != 0 {
				StorageBound::Included(value)
			} else {
				StorageBound::Excluded(value)
			}
		};
		Ok(StorageRange {
			start,
			end,
		})
	}

	fn decode_geometry(&mut self, tag: u8, depth: usize) -> FerrosGrynnResult<Geometry> {
		self.check_depth(depth)?;
		match tag {
			0x21 => self.decode_position().map(Geometry::Point),
			0x22 => self.decode_positions().map(Geometry::LineString),
			0x23 => self.decode_polygon(depth).map(Geometry::Polygon),
			0x24 => self.decode_positions().map(Geometry::MultiPoint),
			0x25 => {
				let count = self.read_fixed_collection_len()?;
				let mut lines = self.allocate_vec(count)?;
				for _ in 0..count {
					lines.push(self.decode_positions()?);
				}
				Ok(Geometry::MultiLineString(lines))
			}
			0x26 => {
				let count = self.read_fixed_collection_len()?;
				let mut polygons = self.allocate_vec(count)?;
				for _ in 0..count {
					polygons.push(self.decode_polygon(depth + 1)?);
				}
				Ok(Geometry::MultiPolygon(polygons))
			}
			0x27 => {
				let count = self.read_fixed_collection_len()?;
				let mut geometries = self.allocate_vec(count)?;
				for _ in 0..count {
					let offset = self.position;
					let child_tag = self.read_u8()?;
					if !(0x21..=0x27).contains(&child_tag) {
						return Err(invalid(
							offset,
							"geometry collection contains a non-geometry tag",
						));
					}
					geometries.push(self.decode_geometry(child_tag, depth + 1)?);
				}
				Ok(Geometry::Collection(geometries))
			}
			_ => Err(invalid(self.position, "unknown geometry tag")),
		}
	}

	fn decode_polygon(&mut self, depth: usize) -> FerrosGrynnResult<Vec<Vec<Position>>> {
		self.check_depth(depth)?;
		let count = self.read_fixed_collection_len()?;
		let mut rings = self.allocate_vec(count)?;
		for _ in 0..count {
			rings.push(self.decode_positions()?);
		}
		Ok(rings)
	}

	fn decode_positions(&mut self) -> FerrosGrynnResult<Vec<Position>> {
		let count = self.read_fixed_collection_len()?;
		let mut positions = self.allocate_vec(count)?;
		for _ in 0..count {
			positions.push(self.decode_position()?);
		}
		Ok(positions)
	}

	fn decode_position(&mut self) -> FerrosGrynnResult<Position> {
		let offset = self.position;
		let longitude = self.read_canonical_f64()?;
		let latitude = self.read_canonical_f64()?;
		self.charge(2 * size_of::<f64>())?;
		Position::new([longitude, latitude])
			.map_err(|cause| invalid(offset, "geometry coordinate is invalid").with_cause(cause))
	}

	fn decode_boxed_value(&mut self, depth: usize) -> FerrosGrynnResult<Box<StorageValue>> {
		self.charge(size_of::<StorageValue>())?;
		self.decode_value(depth).map(Box::new)
	}

	fn decode_vector(&mut self, tag: u8) -> FerrosGrynnResult<Vector> {
		let dimension = usize::from(self.read_u16()?);
		self.check_collection_len(dimension)?;
		match tag {
			0x31 => {
				let mut values = self.allocate_vec(dimension)?;
				for _ in 0..dimension {
					values.push(self.read_canonical_f64()?);
				}
				Ok(Vector::F64(values))
			}
			0x32 => {
				let mut values = self.allocate_vec(dimension)?;
				for _ in 0..dimension {
					values.push(self.read_canonical_f32()?);
				}
				Ok(Vector::F32(values))
			}
			0x33 => self.decode_i64_vector(dimension).map(Vector::I64),
			0x34 => self.decode_i32_vector(dimension).map(Vector::I32),
			0x35 => self.decode_i16_vector(dimension).map(Vector::I16),
			_ => Err(invalid(self.position, "unknown vector tag")),
		}
	}

	fn decode_i64_vector(&mut self, dimension: usize) -> FerrosGrynnResult<Vec<i64>> {
		let mut values = self.allocate_vec(dimension)?;
		for _ in 0..dimension {
			values.push(self.read_i64()?);
		}
		Ok(values)
	}

	fn decode_i32_vector(&mut self, dimension: usize) -> FerrosGrynnResult<Vec<i32>> {
		let mut values = self.allocate_vec(dimension)?;
		for _ in 0..dimension {
			values.push(self.read_i32()?);
		}
		Ok(values)
	}

	fn decode_i16_vector(&mut self, dimension: usize) -> FerrosGrynnResult<Vec<i16>> {
		let mut values = self.allocate_vec(dimension)?;
		for _ in 0..dimension {
			values.push(self.read_i16()?);
		}
		Ok(values)
	}

	fn read_text(&mut self) -> FerrosGrynnResult<Text> {
		let length = self.read_limited_len("string bytes", self.limits.max_string_bytes)?;
		self.charge(length)?;
		let offset = self.position;
		let bytes = self.take(length)?;
		str::from_utf8(bytes).map(Text::from).map_err(|source| {
			invalid(offset, "string is not valid UTF-8").with_technical_source(source)
		})
	}

	fn read_blob(&mut self) -> FerrosGrynnResult<Vec<u8>> {
		let length = self.read_limited_len("blob bytes", self.limits.max_blob_bytes)?;
		self.charge(length)?;
		let bytes = self.take(length)?;
		let mut value = Vec::new();
		value.try_reserve_exact(length).map_err(|source| {
			invalid(self.position, "byte allocation failed").with_technical_source(source)
		})?;
		value.extend_from_slice(bytes);
		Ok(value)
	}

	fn read_uuid(&mut self) -> FerrosGrynnResult<Uuid> {
		self.read_array::<16>().map(uuid::Uuid::from_bytes).map(Uuid::from)
	}

	fn read_ulid(&mut self) -> FerrosGrynnResult<Ulid> {
		self.read_array::<16>().map(u128::from_be_bytes).map(ulid::Ulid::from).map(Ulid::from)
	}

	fn read_collection_len(&mut self) -> FerrosGrynnResult<usize> {
		let count = self.read_uleb()?;
		let count = usize::try_from(count).map_err(|_err| {
			limit_error("collection length", self.limits.max_collection_len, usize::MAX)
		})?;
		self.check_collection_len(count)?;
		Ok(count)
	}

	fn read_fixed_collection_len(&mut self) -> FerrosGrynnResult<usize> {
		let count = self.read_u32()? as usize;
		self.check_collection_len(count)?;
		Ok(count)
	}

	fn check_collection_len(&self, count: usize) -> FerrosGrynnResult<()> {
		if count > self.limits.max_collection_len {
			return Err(limit_error("collection length", self.limits.max_collection_len, count));
		}
		Ok(())
	}

	fn read_limited_len(&mut self, name: &str, maximum: usize) -> FerrosGrynnResult<usize> {
		let length = self.read_uleb()?;
		let length =
			usize::try_from(length).map_err(|_err| limit_error(name, maximum, usize::MAX))?;
		if length > maximum {
			return Err(limit_error(name, maximum, length));
		}
		Ok(length)
	}

	fn check_depth(&self, depth: usize) -> FerrosGrynnResult<()> {
		if depth > self.limits.max_depth {
			return Err(limit_error("nesting depth", self.limits.max_depth, depth));
		}
		Ok(())
	}

	fn allocate_vec<T>(&mut self, count: usize) -> FerrosGrynnResult<Vec<T>> {
		let bytes = count.checked_mul(size_of::<T>()).ok_or_else(|| {
			limit_error("allocation bytes", self.limits.max_total_allocation, usize::MAX)
		})?;
		self.charge(bytes)?;
		let mut values = Vec::new();
		values.try_reserve_exact(count).map_err(|source| {
			invalid(self.position, "collection allocation failed").with_technical_source(source)
		})?;
		Ok(values)
	}

	fn charge(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		let total = self.allocated.checked_add(bytes).ok_or_else(|| {
			limit_error("allocation bytes", self.limits.max_total_allocation, usize::MAX)
		})?;
		if total > self.limits.max_total_allocation {
			return Err(limit_error("allocation bytes", self.limits.max_total_allocation, total));
		}
		self.allocated = total;
		Ok(())
	}

	fn read_uleb(&mut self) -> FerrosGrynnResult<u64> {
		let offset = self.position;
		let mut value = 0_u64;
		for index in 0..10 {
			let byte = self.read_u8()?;
			let payload = u64::from(byte & 0x7F);
			if index == 9 && payload > 1 {
				return Err(invalid(offset, "uLEB128 overflows u64"));
			}
			value |= payload << (index * 7);
			if byte & 0x80 == 0 {
				if index > 0 && payload == 0 {
					return Err(invalid(offset, "uLEB128 is not shortest form"));
				}
				return Ok(value);
			}
		}
		Err(invalid(offset, "uLEB128 exceeds ten bytes"))
	}

	fn read_canonical_f64(&mut self) -> FerrosGrynnResult<f64> {
		let offset = self.position;
		let bits = self.read_u64()?;
		let value = f64::from_bits(bits);
		if canonical_f64(value) != bits {
			return Err(invalid(offset, "f64 bits are not canonical"));
		}
		Ok(value)
	}

	fn read_canonical_f32(&mut self) -> FerrosGrynnResult<f32> {
		let offset = self.position;
		let bits = self.read_u32()?;
		let value = f32::from_bits(bits);
		if canonical_f32(value) != bits {
			return Err(invalid(offset, "f32 bits are not canonical"));
		}
		Ok(value)
	}

	fn read_u8(&mut self) -> FerrosGrynnResult<u8> {
		let offset = self.position;
		let byte = self.bytes.get(self.position).copied().ok_or_else(|| truncated(offset))?;
		self.position += 1;
		Ok(byte)
	}

	fn read_u16(&mut self) -> FerrosGrynnResult<u16> {
		self.read_array().map(u16::from_le_bytes)
	}

	fn read_i16(&mut self) -> FerrosGrynnResult<i16> {
		self.read_array().map(i16::from_le_bytes)
	}

	fn read_u32(&mut self) -> FerrosGrynnResult<u32> {
		self.read_array().map(u32::from_le_bytes)
	}

	fn read_i32(&mut self) -> FerrosGrynnResult<i32> {
		self.read_array().map(i32::from_le_bytes)
	}

	fn read_u64(&mut self) -> FerrosGrynnResult<u64> {
		self.read_array().map(u64::from_le_bytes)
	}

	fn read_i64(&mut self) -> FerrosGrynnResult<i64> {
		self.read_array().map(i64::from_le_bytes)
	}

	fn read_i128(&mut self) -> FerrosGrynnResult<i128> {
		self.read_array().map(i128::from_le_bytes)
	}

	fn read_array<const LENGTH: usize>(&mut self) -> FerrosGrynnResult<[u8; LENGTH]> {
		let bytes = self.take(LENGTH)?;
		let mut result = [0_u8; LENGTH];
		result.copy_from_slice(bytes);
		Ok(result)
	}

	fn take(&mut self, length: usize) -> FerrosGrynnResult<&'a [u8]> {
		let offset = self.position;
		let end = offset.checked_add(length).ok_or_else(|| truncated(offset))?;
		let bytes = self.bytes.get(offset..end).ok_or_else(|| truncated(offset))?;
		self.position = end;
		Ok(bytes)
	}
}

/// Versioned framing for checked rkyv snapshot and cache payloads.
pub struct ArchiveEnvelope;

impl ArchiveEnvelope {
	pub fn wrap(schema_id: u32, payload: &[u8]) -> FerrosGrynnResult<AlignedVec> {
		let payload_length = u64::try_from(payload.len()).map_err(|_err| {
			FerrosGrynnError::invalid_archive_envelope("payload length exceeds u64")
		})?;
		let capacity = ARCHIVE_HEADER_LEN.checked_add(payload.len()).ok_or_else(|| {
			FerrosGrynnError::invalid_archive_envelope("archive length overflows usize")
		})?;
		if capacity > AlignedVec::<16>::MAX_CAPACITY {
			return Err(FerrosGrynnError::invalid_archive_envelope(
				"archive exceeds the aligned buffer capacity",
			));
		}
		let mut bytes = AlignedVec::with_capacity(capacity);
		bytes.extend_from_slice(&ARCHIVE_MAGIC);
		bytes.extend_from_slice(&ARCHIVE_FORMAT_VERSION.to_le_bytes());
		bytes.extend_from_slice(&schema_id.to_le_bytes());
		bytes.extend_from_slice(&payload_length.to_le_bytes());
		bytes.extend_from_slice(&crc32(payload).to_le_bytes());
		bytes.extend_from_slice(&[0_u8; ARCHIVE_RESERVED_LEN]);
		bytes.extend_from_slice(payload);
		Ok(bytes)
	}

	pub fn open(bytes: &[u8], expected_schema_id: u32) -> FerrosGrynnResult<&[u8]> {
		if bytes.len() < ARCHIVE_HEADER_LEN {
			return Err(FerrosGrynnError::invalid_archive_envelope("header is truncated"));
		}
		let magic = envelope_array::<4>(bytes, 0)?;
		if magic != ARCHIVE_MAGIC {
			return Err(FerrosGrynnError::invalid_archive_envelope("magic does not match FGAR"));
		}
		let version = u16::from_le_bytes(envelope_array(bytes, 4)?);
		if version != ARCHIVE_FORMAT_VERSION {
			return Err(FerrosGrynnError::invalid_archive_envelope(format!(
				"format version {version} is not supported"
			)));
		}
		let schema_id = u32::from_le_bytes(envelope_array(bytes, 6)?);
		if schema_id != expected_schema_id {
			return Err(FerrosGrynnError::invalid_archive_envelope(format!(
				"schema {schema_id} does not match expected schema {expected_schema_id}"
			)));
		}
		let payload_length = u64::from_le_bytes(envelope_array(bytes, 10)?);
		let payload_length = usize::try_from(payload_length).map_err(|_err| {
			FerrosGrynnError::invalid_archive_envelope("payload length exceeds usize")
		})?;
		let expected_length = ARCHIVE_HEADER_LEN.checked_add(payload_length).ok_or_else(|| {
			FerrosGrynnError::invalid_archive_envelope("archive length overflows usize")
		})?;
		if bytes.len() != expected_length {
			return Err(FerrosGrynnError::invalid_archive_envelope(
				"payload length does not match the envelope",
			));
		}
		let expected_checksum = u32::from_le_bytes(envelope_array(bytes, 18)?);
		if bytes
			.get(22..ARCHIVE_HEADER_LEN)
			.is_none_or(|reserved| reserved.iter().any(|byte| *byte != 0))
		{
			return Err(FerrosGrynnError::invalid_archive_envelope(
				"reserved header bytes are non-zero",
			));
		}
		let payload = bytes
			.get(ARCHIVE_HEADER_LEN..)
			.ok_or_else(|| FerrosGrynnError::invalid_archive_envelope("payload is truncated"))?;
		if payload.as_ptr().align_offset(AlignedVec::<16>::ALIGNMENT) != 0 {
			return Err(FerrosGrynnError::invalid_archive_envelope(
				"payload is not aligned for the archive schema",
			));
		}
		if crc32(payload) != expected_checksum {
			return Err(FerrosGrynnError::invalid_archive_envelope(
				"payload checksum does not match",
			));
		}
		Ok(payload)
	}
}

fn envelope_array<const LENGTH: usize>(
	bytes: &[u8],
	offset: usize,
) -> FerrosGrynnResult<[u8; LENGTH]> {
	let end = offset.checked_add(LENGTH).ok_or_else(|| {
		FerrosGrynnError::invalid_archive_envelope("header offset overflows usize")
	})?;
	let source = bytes
		.get(offset..end)
		.ok_or_else(|| FerrosGrynnError::invalid_archive_envelope("header is truncated"))?;
	let mut value = [0_u8; LENGTH];
	value.copy_from_slice(source);
	Ok(value)
}

fn crc32(bytes: &[u8]) -> u32 {
	let mut crc = u32::MAX;
	for byte in bytes {
		crc ^= u32::from(*byte);
		for _ in 0..8 {
			let mask = 0_u32.wrapping_sub(crc & 1);
			crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
		}
	}
	!crc
}

fn canonical_f32(value: f32) -> u32 {
	if value.is_nan() {
		f32::NAN.to_bits()
	} else if value == 0.0 {
		0.0_f32.to_bits()
	} else {
		value.to_bits()
	}
}

fn truncated(offset: usize) -> FerrosGrynnError {
	invalid(offset, "encoded value is truncated")
}

fn invalid(offset: usize, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_value_encoding(offset as u64, reason)
}

fn limit_error(name: &str, maximum: usize, actual: usize) -> FerrosGrynnError {
	FerrosGrynnError::storage_decode_limit_exceeded(name, maximum as u64, actual as u64)
}

#[cfg(test)]
mod tests {
	use proptest::prelude::*;

	use super::{ArchiveEnvelope, CanonicalCodec, DecodeLimits, crc32};
	use crate::{
		gql_status::FerrosGrynnError,
		values::{
			Geometry,
			Position,
			PropertyKeyId,
			Text,
			Uuid,
			Vector,
			storage::{
				DiskAtomRef,
				DiskGraphRef,
				QualifiedAtomRef,
				StorageBound,
				StorageDateTime,
				StorageDecimal,
				StorageDuration,
				StorageRange,
				StorageRecordId,
				StorageRecordIdPart,
				StorageRecordValue,
				StorageValue,
				TypeTag,
			},
		},
	};

	#[test]
	fn all_format_v1_tags_round_trip() {
		let point = Position::new([10.0, 20.0]).unwrap();
		let second = Position::new([-10.0, -20.0]).unwrap();
		let atom = DiskAtomRef::from_parts(crate::values::AtomKind::Edge, 17).unwrap();
		let values = vec![
			StorageValue::None,
			StorageValue::Null,
			StorageValue::Bool(false),
			StorageValue::Bool(true),
			StorageValue::Int(i64::MIN),
			StorageValue::Float(1.5_f64.to_bits()),
			StorageValue::Decimal(StorageDecimal {
				mantissa: -123,
				scale: 2,
			}),
			StorageValue::String(Text::from("hello")),
			StorageValue::Bytes([0_u8, 255].into()),
			StorageValue::DateTime(StorageDateTime {
				seconds: -1,
				nanos: 500_000_000,
				time_zone_id: 0,
			}),
			StorageValue::Duration(StorageDuration {
				negative: true,
				units: [0, 0, 0, 0, 0, 0, 30, 0, 0, 0],
			}),
			StorageValue::Uuid(Uuid::new_v4()),
			StorageValue::Ulid(crate::values::Ulid::new()),
			StorageValue::Record(StorageRecordId {
				table_id: 3,
				part: StorageRecordIdPart::Object(vec![
					(Text::from("a"), StorageRecordValue::Int(1)),
					(
						Text::from("b"),
						StorageRecordValue::Array(vec![StorageRecordValue::String("x".into())]),
					),
				]),
			}),
			StorageValue::Array(vec![StorageValue::Int(1), StorageValue::String("x".into())]),
			StorageValue::Set(vec![StorageValue::Int(1), StorageValue::String("x".into())]),
			StorageValue::Object(vec![
				(PropertyKeyId::new(1), StorageValue::Null),
				(PropertyKeyId::new(2), StorageValue::Int(2)),
			]),
			StorageValue::Range(StorageRange {
				start: StorageBound::Included(Box::new(StorageValue::Int(1))),
				end: StorageBound::Excluded(Box::new(StorageValue::Int(3))),
			}),
			StorageValue::Geometry(Geometry::Point(point.clone())),
			StorageValue::Geometry(Geometry::LineString(vec![point.clone(), second.clone()])),
			StorageValue::Geometry(Geometry::Polygon(vec![vec![point.clone(), second.clone()]])),
			StorageValue::Geometry(Geometry::MultiPoint(vec![point.clone(), second.clone()])),
			StorageValue::Geometry(Geometry::MultiLineString(vec![vec![
				point.clone(),
				second.clone(),
			]])),
			StorageValue::Geometry(Geometry::MultiPolygon(vec![vec![vec![point.clone(), second]]])),
			StorageValue::Geometry(Geometry::Collection(vec![Geometry::Point(point)])),
			StorageValue::Vector(Vector::F64(vec![1.0, 2.0])),
			StorageValue::Vector(Vector::F32(vec![1.0, 2.0])),
			StorageValue::Vector(Vector::I64(vec![1, 2])),
			StorageValue::Vector(Vector::I32(vec![1, 2])),
			StorageValue::Vector(Vector::I16(vec![1, 2])),
			StorageValue::AtomRef(atom),
			StorageValue::GraphRef(DiskGraphRef::new(7, 2)),
			StorageValue::QualifiedAtomRef(QualifiedAtomRef::new(9, atom)),
		];

		for value in values {
			let encoded = CanonicalCodec::encode(value.as_ref()).unwrap();
			let decoded = CanonicalCodec::decode(&encoded, DecodeLimits::default()).unwrap();
			assert_eq!(decoded, value);
		}
	}

	#[test]
	fn rejects_unknown_tags_truncation_noncanonical_leb_and_utf8() {
		assert_status(
			CanonicalCodec::decode(&[0x14], DecodeLimits::default()).unwrap_err(),
			"FG-22S01",
		);
		assert_status(
			CanonicalCodec::decode(&[TypeTag::Float.byte(), 0], DecodeLimits::default())
				.unwrap_err(),
			"FG-22S02",
		);
		assert_status(
			CanonicalCodec::decode(&[TypeTag::Int.byte(), 0x80, 0x00], DecodeLimits::default())
				.unwrap_err(),
			"FG-22S02",
		);
		assert_status(
			CanonicalCodec::decode(&[TypeTag::String.byte(), 1, 0xFF], DecodeLimits::default())
				.unwrap_err(),
			"FG-22S02",
		);
	}

	#[test]
	fn rejects_duplicate_or_unsorted_set_object_and_record_keys() {
		let duplicate_set = [TypeTag::Set.byte(), 2, TypeTag::Null.byte(), TypeTag::Null.byte()];
		assert_status(
			CanonicalCodec::decode(&duplicate_set, DecodeLimits::default()).unwrap_err(),
			"FG-22S02",
		);

		let object = [
			TypeTag::Object.byte(),
			2,
			2,
			0,
			0,
			0,
			TypeTag::Null.byte(),
			1,
			0,
			0,
			0,
			TypeTag::Null.byte(),
		];
		assert_status(
			CanonicalCodec::decode(&object, DecodeLimits::default()).unwrap_err(),
			"FG-22S02",
		);

		let record = [
			TypeTag::RecordId.byte(),
			0,
			0,
			0,
			0,
			0x05,
			2,
			1,
			b'b',
			TypeTag::Int.byte(),
			0,
			1,
			b'a',
			TypeTag::Int.byte(),
			0,
		];
		assert_status(
			CanonicalCodec::decode(&record, DecodeLimits::default()).unwrap_err(),
			"FG-22S02",
		);
	}

	#[test]
	fn enforces_depth_length_and_allocation_limits_before_reserve() {
		let nested = [TypeTag::Array.byte(), 1, TypeTag::Array.byte(), 1, TypeTag::Null.byte()];
		let limits = DecodeLimits::new(1, 10, 10, 10, 1024);
		assert_status(CanonicalCodec::decode(&nested, limits).unwrap_err(), "FG-54S01");

		let long = [TypeTag::String.byte(), 4, b't', b'e', b's', b't'];
		let limits = DecodeLimits::new(10, 10, 3, 10, 1024);
		assert_status(CanonicalCodec::decode(&long, limits).unwrap_err(), "FG-54S01");

		let array = [TypeTag::Array.byte(), 2, TypeTag::Null.byte(), TypeTag::Null.byte()];
		let limits = DecodeLimits::new(10, 10, 10, 10, 1);
		assert_status(CanonicalCodec::decode(&array, limits).unwrap_err(), "FG-54S01");
	}

	#[test]
	fn archive_envelope_checks_magic_version_schema_length_and_checksum() {
		let payload = b"checked rkyv payload";
		let archive = ArchiveEnvelope::wrap(42, payload).unwrap();
		let opened = ArchiveEnvelope::open(&archive, 42).unwrap();
		assert_eq!(opened, payload);
		assert_eq!(opened.as_ptr() as usize % rkyv::util::AlignedVec::<16>::ALIGNMENT, 0);

		let mut corrupted = archive.clone();
		let last = corrupted.len() - 1;
		corrupted[last] ^= 1;
		assert_status(ArchiveEnvelope::open(&corrupted, 42).unwrap_err(), "FG-22S06");
		assert_status(ArchiveEnvelope::open(&archive, 7).unwrap_err(), "FG-22S06");
		let mut reserved = archive.clone();
		reserved[22] = 1;
		assert_status(ArchiveEnvelope::open(&reserved, 42).unwrap_err(), "FG-22S06");
		let mut misaligned = rkyv::util::AlignedVec::<16>::new();
		misaligned.push(0);
		misaligned.extend_from_slice(&archive);
		assert_status(ArchiveEnvelope::open(&misaligned[1..], 42).unwrap_err(), "FG-22S06");
		assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
	}

	#[test]
	fn range_flag_validation_rejects_reserved_and_inclusive_unbounded_forms() {
		for flags in [0x10, 0x05] {
			let bytes = [TypeTag::Range.byte(), flags];
			assert_status(
				CanonicalCodec::decode(&bytes, DecodeLimits::default()).unwrap_err(),
				"FG-22S02",
			);
		}
	}

	#[test]
	fn canonical_float_encoding_rejects_signed_zero_and_nan_payloads() {
		for bits in [(-0.0_f64).to_bits(), 0x7FF8_0000_0000_0001] {
			let mut bytes = vec![TypeTag::Float.byte()];
			bytes.extend_from_slice(&bits.to_le_bytes());
			assert_status(
				CanonicalCodec::decode(&bytes, DecodeLimits::default()).unwrap_err(),
				"FG-22S02",
			);
		}
	}

	#[test]
	fn duration_rejects_reserved_bits_zero_units_and_negative_zero() {
		for bytes in [
			vec![TypeTag::Duration.byte(), 1, 0],
			vec![TypeTag::Duration.byte(), 0, 0x40, 0],
			vec![TypeTag::Duration.byte(), 0, 0x80],
		] {
			assert_status(
				CanonicalCodec::decode(&bytes, DecodeLimits::default()).unwrap_err(),
				"FG-22S02",
			);
		}
	}

	proptest! {
		#[test]
		fn decoder_never_panics_on_arbitrary_input(
			bytes in proptest::collection::vec(any::<u8>(), 0..512),
		) {
			let limits = DecodeLimits::new(16, 256, 256, 256, 16 * 1024);
			let _result = CanonicalCodec::decode(&bytes, limits);
		}
	}

	fn assert_status(error: FerrosGrynnError, expected: &str) {
		assert_eq!(error.status().code().to_string(), expected);
	}
}
