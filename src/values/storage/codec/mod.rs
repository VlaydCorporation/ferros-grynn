#![allow(
	clippy::missing_docs_in_private_items,
	reason = "private codec primitives directly implement the documented format table and are \
	          tested through CanonicalCodec"
)]

//! Canonical format-version-1 codec and checked archive envelope.

mod decode;
mod encode;
mod envelope;

pub(super) use encode::canonicalize_set;
#[allow(
	unused_imports,
	reason = "archive framing is part of the crate-private storage codec facade"
)]
pub use envelope::ArchiveEnvelope;
#[cfg(test)]
use envelope::crc32;

use super::{StorageValue, StorageValueRef};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

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
		encode::encode_value(&mut output, &value)?;
		Ok(output)
	}

	pub fn decode(bytes: &[u8], limits: DecodeLimits) -> FerrosGrynnResult<StorageValue> {
		let (value, consumed) = Self::decode_prefix(bytes, limits)?;
		if consumed != bytes.len() {
			return Err(invalid(consumed, "trailing bytes after StorageValue"));
		}
		Ok(value)
	}

	/// Decodes one self-delimiting value from the beginning of a larger
	/// canonical record.
	pub(crate) fn decode_prefix(
		bytes: &[u8],
		limits: DecodeLimits,
	) -> FerrosGrynnResult<(StorageValue, usize)> {
		let mut decoder = decode::Decoder::new(bytes, limits);
		let value = decoder.decode_value(0)?;
		Ok((value, decoder.position))
	}
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
			GraphId,
			Position,
			PropertyKeyId,
			Text,
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
				TypeTag,
			},
		},
	};

	#[test]
	fn all_format_v1_tags_round_trip() {
		let point = Position::new([10.0, 20.0]).unwrap();
		let second = Position::new([-10.0, -20.0]).unwrap();
		let local_atom = DiskAtomRef::from_parts(crate::values::AtomKind::Edge, 17).unwrap();
		let atom = StorageAtomRef {
			kind: crate::values::AtomKind::Edge,
			record_id: StorageRecordId {
				class_id: 3,
				part: StorageRecordIdPart::Int(17),
			},
		};
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
				class_id: 3,
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
			StorageValue::GraphRef(DiskGraphRef::new(GraphId::new(7).unwrap())),
			StorageValue::QualifiedAtomRef(QualifiedAtomRef::new(9, local_atom)),
		];

		for value in values {
			let encoded = CanonicalCodec::encode(value.as_ref()).unwrap();
			let decoded = CanonicalCodec::decode(&encoded, DecodeLimits::default()).unwrap();
			assert_eq!(decoded, value);
		}
	}

	#[test]
	fn atom_reference_uses_kind_and_canonical_record_id_bytes() {
		let value = StorageValue::AtomRef(StorageAtomRef {
			kind: crate::values::AtomKind::Edge,
			record_id: StorageRecordId {
				class_id: 3,
				part: StorageRecordIdPart::Int(4),
			},
		});
		assert_eq!(CanonicalCodec::encode(value.as_ref()).unwrap(), [
			TypeTag::AtomRef.byte(),
			1,
			TypeTag::RecordId.byte(),
			3,
			0,
			0,
			0,
			0,
			8
		]);
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
