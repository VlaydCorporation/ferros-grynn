use std::{mem::size_of, str};

use super::{
	DURATION_SIGN,
	DURATION_UNIT_MASK,
	DecodeLimits,
	RANGE_ALLOWED_FLAGS,
	RANGE_LOWER_INCLUSIVE,
	RANGE_LOWER_UNBOUNDED,
	RANGE_UPPER_INCLUSIVE,
	RANGE_UPPER_UNBOUNDED,
	canonical_f32,
	encode::zigzag_decode,
	invalid,
	limit_error,
	truncated,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{
		Bytes,
		Geometry,
		Position,
		PropertyKeyId,
		Text,
		Ulid,
		Uuid,
		Vector,
		storage::{
			DiskAtomRef,
			DiskGraphRef,
			QualifiedAtomRef,
			StorageAtomRef,
			StorageBound,
			StorageDateTime,
			StorageDecimal,
			StorageDuration,
			StorageRange,
			StorageRecordId,
			StorageRecordIdPart,
			StorageRecordValue,
			StorageValue,
			canonical_f64,
		},
	},
};

pub(super) struct Decoder<'a> {
	bytes: &'a [u8],
	pub(super) position: usize,
	limits: DecodeLimits,
	allocated: usize,
}

impl<'a> Decoder<'a> {
	pub(super) const fn new(bytes: &'a [u8], limits: DecodeLimits) -> Self {
		Self {
			bytes,
			position: 0,
			limits,
			allocated: 0,
		}
	}

	pub(super) fn decode_value(&mut self, depth: usize) -> FerrosGrynnResult<StorageValue> {
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
				let kind = match self.read_u8()? {
					0 => crate::values::AtomKind::Node,
					1 => crate::values::AtomKind::Edge,
					_ => return Err(invalid(offset, "atom reference contains an unknown kind")),
				};
				if self.read_u8()? != crate::values::storage::TypeTag::RecordId.byte() {
					return Err(invalid(
						offset,
						"atom reference does not contain a canonical RecordId",
					));
				}
				let record_id = self.decode_record_id(depth + 1)?;
				Ok(StorageValue::AtomRef(StorageAtomRef {
					kind,
					record_id,
				}))
			}
			0x37 => DiskGraphRef::from_raw(self.read_u64()?).map(StorageValue::GraphRef),
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
		let class_id = self.read_u32()?;
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
			class_id,
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
