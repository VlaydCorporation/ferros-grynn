//! Allocation-free sizing of canonical bytes and encoder-owned temporary work.

use super::{encode::zigzag_encode, invalid};
use crate::{
	gql_status::FerrosGrynnResult,
	values::{
		Geometry,
		Position,
		Text,
		Vector,
		storage::{
			StorageBound,
			StorageRecordId,
			StorageRecordIdPart,
			StorageRecordValue,
			StorageValue,
		},
	},
};

/// Output capacity and temporary allocations have different lifetimes.
#[derive(Clone, Copy, Debug, Default)]
pub struct EncodingLayout {
	/// Exact canonical output length, including the leading value tag.
	pub(crate) encoded_len: usize,
	/// Peak reserved-encoder scratch, excluding the caller's DTO and output.
	pub(crate) scratch_bytes: usize,
}

/// Measures borrowed DTOs without encoding Set keys or entering an allocating
/// geometry kernel. Owner shape/resource checks precede output reservation.
pub(super) fn value_layout(value: &StorageValue) -> FerrosGrynnResult<EncodingLayout> {
	let mut layout = EncodingLayout::default();
	layout.encoded_len = match value {
		StorageValue::None | StorageValue::Null | StorageValue::Bool(_) => 1,
		StorageValue::Int(value) => add(1, uleb_len(zigzag_encode(*value)))?,
		StorageValue::Float(_) => 9,
		StorageValue::Decimal(_) => 18,
		StorageValue::String(value) => add(1, bytes_len(value.len())?)?,
		StorageValue::Bytes(value) => add(1, bytes_len(value.len())?)?,
		StorageValue::DateTime(_) => 17,
		StorageValue::Duration(value) => {
			let mut bytes = 3;
			for unit in value.units.iter().filter(|unit| **unit != 0) {
				bytes = add(bytes, uleb_len(*unit))?;
			}
			bytes
		}
		StorageValue::Uuid(_) | StorageValue::Ulid(_) => 17,
		StorageValue::Record(value) => record_len(value)?,
		StorageValue::Array(values) | StorageValue::Set(values) => {
			let is_set = matches!(value, StorageValue::Set(_));
			let mut bytes = add(1, count_len(values.len())?)?;
			let mut previous = 0;
			for value in values {
				let child = value_layout(value)?;
				bytes = add(bytes, child.encoded_len)?;
				// Set holds previous/current complete child bytes while
				// encoding.
				let scratch = if is_set {
					add(add(previous, child.encoded_len)?, child.scratch_bytes)?
				} else {
					child.scratch_bytes
				};
				layout.scratch_bytes = layout.scratch_bytes.max(scratch);
				previous = child.encoded_len;
			}
			bytes
		}
		StorageValue::Object(values) => {
			let mut bytes = add(1, count_len(values.len())?)?;
			for (_, value) in values {
				let child = value_layout(value)?;
				bytes = add(bytes, add(4, child.encoded_len)?)?;
				layout.scratch_bytes = layout.scratch_bytes.max(child.scratch_bytes);
			}
			bytes
		}
		StorageValue::Range(value) => {
			let mut bytes = 2;
			for bound in [&value.start, &value.end] {
				if let StorageBound::Included(value) | StorageBound::Excluded(value) = bound {
					let child = value_layout(value)?;
					bytes = add(bytes, child.encoded_len)?;
					layout.scratch_bytes = layout.scratch_bytes.max(child.scratch_bytes);
				}
			}
			bytes
		}
		StorageValue::Geometry(value) => {
			layout.scratch_bytes = value.validation_scratch_bytes()?;
			geometry_len(value)?
		}
		StorageValue::Vector(value) => {
			u16::try_from(value.len())
				.map_err(|_error| invalid(0, "vector dimension exceeds u16"))?;
			let width = match value {
				Vector::F64(_) | Vector::I64(_) => 8,
				Vector::F32(_) | Vector::I32(_) => 4,
				Vector::F16(_) | Vector::I16(_) => 2,
				Vector::I8(_) | Vector::U8(_) => 1,
			};
			add(3, mul(value.len(), width)?)?
		}
		StorageValue::AtomRef(value) => add(2, record_len(&value.record_id)?)?,
		StorageValue::GraphRef(_) => 9,
		// The current value DTO stores a u32 graph slot and a u64 local ref.
		StorageValue::QualifiedAtomRef(_) => 13,
	};
	Ok(layout)
}

/// RecordId part tags replace the ordinary value tags at the top level.
fn record_len(value: &StorageRecordId) -> FerrosGrynnResult<usize> {
	let part = match &value.part {
		StorageRecordIdPart::Int(value) => uleb_len(zigzag_encode(*value)),
		StorageRecordIdPart::String(value) => bytes_len(value.len())?,
		StorageRecordIdPart::Uuid(_) | StorageRecordIdPart::Ulid(_) => 16,
		StorageRecordIdPart::Array(values) => record_array_len(values)?,
		StorageRecordIdPart::Object(values) => record_object_len(values)?,
	};
	add(6, part)
}

/// Nested RecordId values use ordinary scalar/container tags.
fn record_value_len(value: &StorageRecordValue) -> FerrosGrynnResult<usize> {
	let body = match value {
		StorageRecordValue::Int(value) => uleb_len(zigzag_encode(*value)),
		StorageRecordValue::String(value) => bytes_len(value.len())?,
		StorageRecordValue::Uuid(_) | StorageRecordValue::Ulid(_) => 16,
		StorageRecordValue::Array(values) => record_array_len(values)?,
		StorageRecordValue::Object(values) => record_object_len(values)?,
	};
	add(1, body)
}

/// Counts a RecordId sequence without constructing a second representation.
fn record_array_len(values: &[StorageRecordValue]) -> FerrosGrynnResult<usize> {
	let mut bytes = count_len(values.len())?;
	for value in values {
		bytes = add(bytes, record_value_len(value)?)?;
	}
	Ok(bytes)
}

/// RecordId object names remain inline UTF-8 rather than dictionary IDs.
fn record_object_len(values: &[(Text, StorageRecordValue)]) -> FerrosGrynnResult<usize> {
	let mut bytes = count_len(values.len())?;
	for (key, value) in values {
		bytes = add(bytes, add(bytes_len(key.len())?, record_value_len(value)?)?)?;
	}
	Ok(bytes)
}

/// Counts geometry bytes; the owner supplies the separate validation scratch.
fn geometry_len(value: &Geometry) -> FerrosGrynnResult<usize> {
	let body = match value {
		Geometry::Point(value) => mul(value.as_slice().len(), 8)?,
		Geometry::LineString(values) | Geometry::MultiPoint(values) => positions_len(values)?,
		Geometry::Polygon(rings) | Geometry::MultiLineString(rings) => rings_len(rings)?,
		Geometry::MultiPolygon(polygons) => {
			let mut bytes = geometry_count(polygons.len())?;
			for polygon in polygons {
				bytes = add(bytes, rings_len(polygon)?)?;
			}
			bytes
		}
		Geometry::Collection(values) => {
			let mut bytes = geometry_count(values.len())?;
			for value in values {
				bytes = add(bytes, geometry_len(value)?)?;
			}
			bytes
		}
	};
	add(1, body)
}

/// Counts the shared polygon/line-family container body.
fn rings_len(rings: &[Vec<Position>]) -> FerrosGrynnResult<usize> {
	let mut bytes = geometry_count(rings.len())?;
	for ring in rings {
		bytes = add(bytes, positions_len(ring)?)?;
	}
	Ok(bytes)
}

/// Positions retain their actual coordinate count until semantic validation.
fn positions_len(values: &[Position]) -> FerrosGrynnResult<usize> {
	let mut bytes = geometry_count(values.len())?;
	for value in values {
		bytes = add(bytes, mul(value.as_slice().len(), 8)?)?;
	}
	Ok(bytes)
}

/// Geometry containers have a fixed u32 count, checked before output reserve.
fn geometry_count(count: usize) -> FerrosGrynnResult<usize> {
	u32::try_from(count).map_err(|_error| invalid(0, "geometry count exceeds u32"))?;
	Ok(4)
}

/// Strings and binary payloads share the same length-prefixed framing.
fn bytes_len(bytes: usize) -> FerrosGrynnResult<usize> {
	add(count_len(bytes)?, bytes)
}

/// Canonical collection counts use unsigned LEB128.
fn count_len(count: usize) -> FerrosGrynnResult<usize> {
	Ok(uleb_len(u64::try_from(count).map_err(|_error| invalid(0, "length exceeds u64"))?))
}

/// Zero still occupies one LEB128 byte.
const fn uleb_len(mut value: u64) -> usize {
	let mut bytes = 1;
	while value >= 0x80 {
		bytes += 1;
		value >>= 7;
	}
	bytes
}

/// Size arithmetic must fail before it can authorize a smaller allocation.
fn add(left: usize, right: usize) -> FerrosGrynnResult<usize> {
	left.checked_add(right).ok_or_else(|| invalid(0, "canonical encoding size overflows usize"))
}

/// Checks dimension and container byte multiplication before allocation.
fn mul(left: usize, right: usize) -> FerrosGrynnResult<usize> {
	left.checked_mul(right).ok_or_else(|| invalid(0, "canonical encoding size overflows usize"))
}

#[cfg(test)]
/// The existing growing encoder is the byte oracle for the admitted path.
mod tests {
	use super::*;
	use crate::{
		gql_status::Status,
		test_assertions::AssertResult as _,
		values::storage::{DiskAtomRef, QualifiedAtomRef, codec::CanonicalCodec},
	};

	/// LEB boundaries must agree with emitted lengths for strings and integers.
	#[test]
	fn reserved_encoding_layout_matches_length_boundaries() {
		for length in [0, 1, 127, 128, 16_335, 16_336, 16_337, 16_383, 16_384] {
			let value = StorageValue::String(Text::from("x".repeat(length)));
			assert_bytes(&value);
		}
		for value in [i64::MIN, -8193, -8192, -65, -64, -1, 0, 63, 64, 8191, 8192, i64::MAX] {
			assert_bytes(&StorageValue::Int(value));
		}
	}

	/// The current value codec qualifies a local ref by a four-byte graph slot.
	#[test]
	fn reserved_encoding_qualified_ref_preserves_current_slot_width() {
		let local = DiskAtomRef::from_parts(crate::values::AtomKind::Node, 17).assert_ok();
		let value = StorageValue::QualifiedAtomRef(QualifiedAtomRef::new(u32::MAX, local));
		let expected = [0x38, 255, 255, 255, 255, 17, 0, 0, 0, 0, 0, 0, 0];
		assert_eq!(CanonicalCodec::encoding_layout(value.as_ref()).assert_ok().encoded_len, 13);
		assert_eq!(CanonicalCodec::encode(value.as_ref()).assert_ok(), expected);
		assert_eq!(CanonicalCodec::encode_bounded(value.as_ref(), 13).assert_ok(), expected);
	}

	/// The previous outer key coexists with both buffers of the inner Set.
	#[test]
	fn reserved_encoding_nested_sets_admit_peak_before_output() {
		let value = StorageValue::Set(vec![
			StorageValue::Int(0),
			StorageValue::Set(vec![
				StorageValue::String(Text::from("a")),
				StorageValue::String(Text::from("b".repeat(255))),
			]),
		]);
		let layout = CanonicalCodec::encoding_layout(value.as_ref()).assert_ok();
		// Inner keys occupy 3 and 258 bytes; its whole image occupies 263.
		assert_eq!((layout.encoded_len, layout.scratch_bytes), (267, 526));
		for maximum in [267, 792] {
			let error = CanonicalCodec::encode_bounded(value.as_ref(), maximum).assert_err();
			assert!(matches!(
				error.status().status(),
				Status::StorageDecodeLimitExceeded { actual: 793, maximum: observed, .. }
					if *observed == maximum as u64
			));
		}
		assert_eq!(
			CanonicalCodec::encode_bounded(value.as_ref(), 793).assert_ok(),
			CanonicalCodec::encode(value.as_ref()).assert_ok()
		);
		assert_bytes(&StorageValue::Array(vec![value]));
	}

	/// Exact reservation cannot bypass canonical Set order validation.
	#[test]
	fn reserved_encoding_rejects_noncanonical_sets() {
		for values in [vec![StorageValue::Int(2), StorageValue::Int(1)], vec![
			StorageValue::Int(1),
			StorageValue::Int(1),
		]] {
			let value = StorageValue::Set(values);
			let old = CanonicalCodec::encode(value.as_ref()).assert_err();
			let bounded = CanonicalCodec::encode_bounded(value.as_ref(), 1024).assert_err();
			assert_eq!(old.status().status(), bounded.status().status());
		}
	}

	/// Arithmetic must reject a layout that would wrap into a small capacity.
	#[test]
	fn reserved_encoding_layout_rejects_size_overflow() {
		assert!(add(usize::MAX, 1).is_err());
		assert!(mul(usize::MAX, 2).is_err());
		assert_eq!(uleb_len(0), 1);
		assert_eq!(uleb_len(u64::MAX), 10);
	}

	/// Compare complete bytes and output length against the existing encoder.
	fn assert_bytes(value: &StorageValue) {
		let encoded = CanonicalCodec::encode(value.as_ref()).assert_ok();
		let layout = CanonicalCodec::encoding_layout(value.as_ref()).assert_ok();
		assert_eq!(layout.encoded_len, encoded.len());
		assert_eq!(
			CanonicalCodec::encode_bounded(
				value.as_ref(),
				layout.encoded_len + layout.scratch_bytes
			)
			.assert_ok(),
			encoded
		);
	}
}
