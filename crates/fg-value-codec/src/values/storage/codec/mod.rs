#![allow(
	clippy::missing_docs_in_private_items,
	reason = "private codec primitives directly implement the documented format table and are \
	          tested through CanonicalCodec"
)]

//! Canonical format-version-1 codec and checked archive envelope.

/// Logical resource policy shared with allocation-free DTO admission.
mod admission;
/// Bounded decoding of canonical storage values.
mod decode;
/// Canonical storage-value encoding and set normalization.
mod encode;
/// Versioned archive framing, length checks and checksum validation.
mod envelope;
/// Exact output length and reserved-encoder temporary allocation layout.
mod layout;
/// Allocation-free admission in the final canonical DTO order.
mod read_limits;

pub(in crate::values::storage) use admission::DecodeAdmission;
pub(super) use encode::canonicalize_set;
pub(in crate::values::storage) use encode::finish_set;
#[allow(
	unused_imports,
	reason = "archive framing is part of the crate-private storage codec facade"
)]
pub use envelope::ArchiveEnvelope;
#[cfg(test)]
use envelope::crc32;
pub use layout::EncodingLayout;
pub(in crate::values::storage) use read_limits::geometry_allocation;

use super::{StorageValue, StorageValueRef};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Range header bit indicating an inclusive lower endpoint.
const RANGE_LOWER_INCLUSIVE: u8 = 1 << 0;
/// Range header bit indicating an inclusive upper endpoint.
const RANGE_UPPER_INCLUSIVE: u8 = 1 << 1;
/// Range header bit indicating that no lower endpoint is stored.
const RANGE_LOWER_UNBOUNDED: u8 = 1 << 2;
/// Range header bit indicating that no upper endpoint is stored.
const RANGE_UPPER_UNBOUNDED: u8 = 1 << 3;
/// Mask of range-header bits accepted by the canonical decoder.
const RANGE_ALLOWED_FLAGS: u8 = 0x0F;
/// Duration header sign bit; a negative duration with no units is rejected.
const DURATION_SIGN: u16 = 1 << 15;
/// Duration header bits marking nonzero units, excluding the sign and reserved
/// bits.
const DURATION_UNIT_MASK: u16 = 0x7FE0;
/// Magic bytes identifying the versioned value archive envelope.
const ARCHIVE_MAGIC: [u8; 4] = *b"FGAR";
/// Value archive envelope version accepted by the codec.
const ARCHIVE_FORMAT_VERSION: u16 = 1;
/// Fixed archive header size in bytes, including its reserved tail.
const ARCHIVE_HEADER_LEN: usize = 32;
/// Reserved archive-header bytes after magic, version, tag, length and
/// checksum.
const ARCHIVE_RESERVED_LEN: usize = ARCHIVE_HEADER_LEN - (4 + 2 + 4 + 8 + 4);

/// Resource bounds applied before allocating from untrusted encoded lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
	/// Maximum nested depth accepted by the operation or declaration.
	pub max_depth: usize,
	/// Maximum element count accepted for a decoded collection.
	pub max_collection_len: usize,
	/// Maximum byte length accepted for one decoded text value.
	pub max_string_bytes: usize,
	/// Maximum byte length accepted for one decoded binary value.
	pub max_blob_bytes: usize,
	/// Maximum combined allocation bytes admitted during decoding.
	pub max_total_allocation: usize,
}

impl DecodeLimits {
	/// Constructs `DecodeLimits` from the supplied initial state.
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
	/// Constructs the default initial state of `DecodeLimits`.
	fn default() -> Self {
		Self::new(64, 1_000_000, 16 * 1024 * 1024, 64 * 1024 * 1024, 128 * 1024 * 1024)
	}
}

/// Canonical value codec defined by `disk_storage_spec.md` section 5.
pub struct CanonicalCodec;

impl CanonicalCodec {
	/// Checks whether a canonical DTO fits an existing reader's resource
	/// policy.
	///
	/// This does not allocate, encode or validate canonical value content. The
	/// return value is the reader's retained logical charge, not a heap quota.
	///
	/// # Errors
	/// Returns the same typed depth, length and allocation limit failures as
	/// decoding the value with the supplied limits.
	pub(crate) fn check_decode_limits(
		value: StorageValueRef<'_>,
		limits: DecodeLimits,
	) -> FerrosGrynnResult<usize> {
		read_limits::check(&value, limits)
	}

	/// Returns encode for `CanonicalCodec`.
	///
	/// # Errors
	/// Returns malformed-value or allocation-limit errors.
	pub fn encode(value: StorageValueRef<'_>) -> FerrosGrynnResult<Vec<u8>> {
		let mut output = Vec::new();
		encode::encode_value(&mut output, &value, false)?;
		Ok(output)
	}

	/// Measures canonical output and peak scratch without materializing bytes.
	///
	/// The caller separately owns the DTO, conversion and dictionary budgets.
	/// Payload canonical validation still runs in the shared encoder; the
	/// layout also applies the geometry owner's shape/resource checks.
	///
	/// # Errors
	/// Returns malformed-value or allocation-limit errors.
	pub fn encoding_layout(value: StorageValueRef<'_>) -> FerrosGrynnResult<EncodingLayout> {
		layout::value_layout(&value)
	}

	/// Admits output plus encoder scratch before reserving canonical bytes.
	///
	/// Nested Set buffers use the same exact-capacity path. This logical limit
	/// excludes the caller's DTO and allocator bookkeeping.
	///
	/// # Errors
	/// Returns typed size, resource, allocation or canonical validation errors.
	pub(crate) fn encode_bounded(
		value: StorageValueRef<'_>,
		maximum_bytes: usize,
	) -> FerrosGrynnResult<Vec<u8>> {
		let layout = Self::encoding_layout(value)?;
		let bytes = layout
			.encoded_len
			.checked_add(layout.scratch_bytes)
			.ok_or_else(|| invalid(0, "canonical encoding memory overflows usize"))?;
		if bytes > maximum_bytes {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"canonical encoding memory",
				bytes as u64,
				maximum_bytes as u64,
			));
		}
		let mut output = Vec::new();
		output
			.try_reserve_exact(layout.encoded_len)
			.map_err(|_error| invalid(0, "canonical output allocation failed"))?;
		encode::encode_value(&mut output, &value, true)?;
		if output.len() != layout.encoded_len {
			return Err(invalid(0, "canonical output disagrees with admitted layout"));
		}
		Ok(output)
	}

	/// Returns decode for `CanonicalCodec`.
	///
	/// # Errors
	/// Returns malformed-value or allocation-limit errors.
	pub fn decode(bytes: &[u8], limits: DecodeLimits) -> FerrosGrynnResult<StorageValue> {
		let (value, consumed) = Self::decode_prefix(bytes, limits)?;
		if consumed != bytes.len() {
			return Err(invalid(consumed, "trailing bytes after StorageValue"));
		}
		Ok(value)
	}

	/// Returns the decoder's admitted allocation for a caller sharing an
	/// aggregate budget across multiple canonical values.
	///
	/// # Errors
	/// Returns malformed-value or allocation-limit errors.
	pub fn decode_with_allocation(
		bytes: &[u8],
		limits: DecodeLimits,
	) -> FerrosGrynnResult<(StorageValue, usize)> {
		let mut decoder = decode::Decoder::new(bytes, limits);
		let value = decoder.decode_value(0)?;
		if decoder.position != bytes.len() {
			return Err(invalid(decoder.position, "trailing bytes after StorageValue"));
		}
		Ok((value, decoder.admission.allocated))
	}

	/// Decodes one self-delimiting value from the beginning of a larger
	/// canonical record.
	///
	/// # Errors
	/// Returns malformed-value or allocation-limit errors.
	pub fn decode_prefix(
		bytes: &[u8],
		limits: DecodeLimits,
	) -> FerrosGrynnResult<(StorageValue, usize)> {
		let mut decoder = decode::Decoder::new(bytes, limits);
		let value = decoder.decode_value(0)?;
		Ok((value, decoder.position))
	}
}

/// Returns canonical f32 for this module.
fn canonical_f32(value: f32) -> u32 {
	if value.is_nan() {
		f32::NAN.to_bits()
	} else if value == 0.0 {
		0.0_f32.to_bits()
	} else {
		value.to_bits()
	}
}

/// Returns truncated for this module.
fn truncated(offset: usize) -> FerrosGrynnError {
	invalid(offset, "encoded value is truncated")
}

/// Returns invalid for this module.
fn invalid(offset: usize, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_value_encoding(offset as u64, reason)
}

/// Maps reader policy order onto the status constructor's observed/maximum
/// order.
fn limit_error(name: &str, maximum: usize, actual: usize) -> FerrosGrynnError {
	FerrosGrynnError::storage_decode_limit_exceeded(name, actual as u64, maximum as u64)
}

#[cfg(test)]
/// Regression tests for codec behavior.
mod tests {
	use proptest::prelude::*;

	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};

	/// Verifies geometry writer and default decoder agree at large point and
	/// depth boundaries.
	#[test]
	fn geometry_writer_and_default_decoder_agree_at_large_point_and_depth_boundaries() {
		let point = crate::values::Position::new([1.0, 2.0]).assert_ok();
		let large = crate::values::storage::StorageValue::Geometry(
			crate::values::Geometry::MultiPoint(vec![point.clone(); 250_000]),
		);
		let bytes = super::CanonicalCodec::encode(large.as_ref()).assert_ok();
		assert_eq!(
			super::CanonicalCodec::decode(&bytes, super::DecodeLimits::default()).assert_ok(),
			large
		);
		assert!(
			super::CanonicalCodec::decode(
				&bytes,
				super::DecodeLimits::new(64, 1_000_000, 1024, 1024, 1024)
			)
			.is_err()
		);
		let mut geometry = crate::values::Geometry::Point(point);
		for _ in 0..64 {
			geometry = crate::values::Geometry::Collection(vec![geometry]);
		}
		let value = crate::values::storage::StorageValue::Geometry(geometry);
		let bytes = super::CanonicalCodec::encode(value.as_ref()).assert_ok();
		assert_eq!(
			super::CanonicalCodec::decode(&bytes, super::DecodeLimits::default()).assert_ok(),
			value
		);
		let mut malformed = vec![0x23];
		malformed.extend_from_slice(&1_u32.to_le_bytes());
		malformed.extend_from_slice(&4_u32.to_le_bytes());
		for (x, y) in [(0.0_f64, 0.0_f64), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
			malformed.extend_from_slice(&x.to_le_bytes());
			malformed.extend_from_slice(&y.to_le_bytes());
		}
		assert!(super::CanonicalCodec::decode(&malformed, super::DecodeLimits::default()).is_err());
	}

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

	/// Verifies all format v1 tags round trip.
	#[test]
	fn all_format_v1_tags_round_trip() {
		let point = Position::new([10.0, 20.0]).assert_ok();
		let second = Position::new([-10.0, -20.0]).assert_ok();
		let ring = vec![
			point.clone(),
			second.clone(),
			Position::new([10.0, -20.0]).assert_ok(),
			point.clone(),
		];
		let local_atom = DiskAtomRef::from_parts(crate::values::AtomKind::Edge, 17).assert_ok();
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
			StorageValue::Geometry(Geometry::Polygon(vec![ring.clone()])),
			StorageValue::Geometry(Geometry::MultiPoint(vec![point.clone(), second.clone()])),
			StorageValue::Geometry(Geometry::MultiLineString(vec![vec![point.clone(), second]])),
			StorageValue::Geometry(Geometry::MultiPolygon(vec![vec![ring]])),
			StorageValue::Geometry(Geometry::Collection(vec![Geometry::Point(point)])),
			StorageValue::Vector(Vector::F64(vec![1.0, 2.0])),
			StorageValue::Vector(Vector::F32(vec![1.0, 2.0])),
			StorageValue::Vector(Vector::F16(vec![
				half::f16::from_f32(-0.0),
				half::f16::from_f32(2.0),
			])),
			StorageValue::Vector(Vector::I64(vec![1, 2])),
			StorageValue::Vector(Vector::I32(vec![1, 2])),
			StorageValue::Vector(Vector::I16(vec![1, 2])),
			StorageValue::Vector(Vector::I8(vec![-1, 2])),
			StorageValue::Vector(Vector::U8(vec![1, 255])),
			StorageValue::AtomRef(atom),
			StorageValue::GraphRef(DiskGraphRef::new(GraphId::new(7).assert_some())),
			StorageValue::QualifiedAtomRef(QualifiedAtomRef::new(9, local_atom)),
		];

		for value in values {
			let encoded = CanonicalCodec::encode(value.as_ref()).assert_ok();
			let layout = CanonicalCodec::encoding_layout(value.as_ref()).assert_ok();
			assert_eq!(layout.encoded_len, encoded.len(), "layout for {value:?}");
			assert_eq!(
				CanonicalCodec::encode_bounded(value.as_ref(), usize::MAX).assert_ok(),
				encoded,
				"bounded bytes for {value:?}"
			);
			let decoded = CanonicalCodec::decode(&encoded, DecodeLimits::default()).assert_ok();
			assert_eq!(decoded, value);
		}
	}

	/// Verifies atom reference uses kind and canonical record id bytes.
	#[test]
	fn atom_reference_uses_kind_and_canonical_record_id_bytes() {
		let value = StorageValue::AtomRef(StorageAtomRef {
			kind: crate::values::AtomKind::Edge,
			record_id: StorageRecordId {
				class_id: 3,
				part: StorageRecordIdPart::Int(4),
			},
		});
		assert_eq!(CanonicalCodec::encode(value.as_ref()).assert_ok(), [
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

	/// Verifies rejects unknown tags truncation noncanonical leb and utf8.
	#[test]
	fn rejects_unknown_tags_truncation_noncanonical_leb_and_utf8() {
		assert_status(
			CanonicalCodec::decode(&[0x14], DecodeLimits::default()).assert_err(),
			"FG-22S01",
		);
		assert_status(
			CanonicalCodec::decode(&[TypeTag::Float.byte(), 0], DecodeLimits::default())
				.assert_err(),
			"FG-22S02",
		);
		assert_status(
			CanonicalCodec::decode(&[TypeTag::Int.byte(), 0x80, 0x00], DecodeLimits::default())
				.assert_err(),
			"FG-22S02",
		);
		assert_status(
			CanonicalCodec::decode(&[TypeTag::String.byte(), 1, 0xFF], DecodeLimits::default())
				.assert_err(),
			"FG-22S02",
		);
	}

	/// Verifies rejects duplicate or unsorted set object and record keys.
	#[test]
	fn rejects_duplicate_or_unsorted_set_object_and_record_keys() {
		let duplicate_set = [TypeTag::Set.byte(), 2, TypeTag::Null.byte(), TypeTag::Null.byte()];
		assert_status(
			CanonicalCodec::decode(&duplicate_set, DecodeLimits::default()).assert_err(),
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
			CanonicalCodec::decode(&object, DecodeLimits::default()).assert_err(),
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
			CanonicalCodec::decode(&record, DecodeLimits::default()).assert_err(),
			"FG-22S02",
		);
	}

	/// Verifies enforces depth length and allocation limits before reserve.
	#[test]
	fn enforces_depth_length_and_allocation_limits_before_reserve() {
		let nested = [TypeTag::Array.byte(), 1, TypeTag::Array.byte(), 1, TypeTag::Null.byte()];
		let limits = DecodeLimits::new(1, 10, 10, 10, 1024);
		assert_status(CanonicalCodec::decode(&nested, limits).assert_err(), "FG-54S01");

		let long = [TypeTag::String.byte(), 4, b't', b'e', b's', b't'];
		let limits = DecodeLimits::new(10, 10, 3, 10, 1024);
		assert_status(CanonicalCodec::decode(&long, limits).assert_err(), "FG-54S01");

		let array = [TypeTag::Array.byte(), 2, TypeTag::Null.byte(), TypeTag::Null.byte()];
		let limits = DecodeLimits::new(10, 10, 10, 10, 1);
		assert_status(CanonicalCodec::decode(&array, limits).assert_err(), "FG-54S01");
	}

	/// Verifies archive envelope checks magic version schema length and
	/// checksum.
	#[test]
	fn archive_envelope_checks_magic_version_schema_length_and_checksum() {
		let payload = b"checked rkyv payload";
		let archive = ArchiveEnvelope::wrap(42, payload).assert_ok();
		let opened = ArchiveEnvelope::open(&archive, 42).assert_ok();
		assert_eq!(opened, payload);
		assert_eq!(opened.as_ptr() as usize % rkyv::util::AlignedVec::<16>::ALIGNMENT, 0);

		let mut corrupted = archive.clone();
		let last = corrupted.len() - 1;
		corrupted[last] ^= 1;
		assert_status(ArchiveEnvelope::open(&corrupted, 42).assert_err(), "FG-22S06");
		assert_status(ArchiveEnvelope::open(&archive, 7).assert_err(), "FG-22S06");
		let mut reserved = archive.clone();
		reserved[22] = 1;
		assert_status(ArchiveEnvelope::open(&reserved, 42).assert_err(), "FG-22S06");
		let mut misaligned = rkyv::util::AlignedVec::<16>::new();
		misaligned.push(0);
		misaligned.extend_from_slice(&archive);
		assert_status(ArchiveEnvelope::open(&misaligned[1..], 42).assert_err(), "FG-22S06");
		assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
	}

	/// Verifies range flag validation rejects reserved and inclusive unbounded
	/// forms.
	#[test]
	fn range_flag_validation_rejects_reserved_and_inclusive_unbounded_forms() {
		for flags in [0x10, 0x05] {
			let bytes = [TypeTag::Range.byte(), flags];
			assert_status(
				CanonicalCodec::decode(&bytes, DecodeLimits::default()).assert_err(),
				"FG-22S02",
			);
		}
	}

	/// Verifies canonical float encoding rejects signed zero and nan payloads.
	#[test]
	fn canonical_float_encoding_rejects_signed_zero_and_nan_payloads() {
		for bits in [(-0.0_f64).to_bits(), 0x7FF8_0000_0000_0001] {
			let mut bytes = vec![TypeTag::Float.byte()];
			bytes.extend_from_slice(&bits.to_le_bytes());
			assert_status(
				CanonicalCodec::decode(&bytes, DecodeLimits::default()).assert_err(),
				"FG-22S02",
			);
		}
	}

	/// Verifies duration rejects reserved bits zero units and negative zero.
	#[test]
	fn duration_rejects_reserved_bits_zero_units_and_negative_zero() {
		for bytes in [
			vec![TypeTag::Duration.byte(), 1, 0],
			vec![TypeTag::Duration.byte(), 0, 0x40, 0],
			vec![TypeTag::Duration.byte(), 0, 0x80],
		] {
			assert_status(
				CanonicalCodec::decode(&bytes, DecodeLimits::default()).assert_err(),
				"FG-22S02",
			);
		}
	}

	proptest! {
		/// Verifies decoder never panics on arbitrary input.
		#[test]
		fn decoder_never_panics_on_arbitrary_input(
			bytes in proptest::collection::vec(any::<u8>(), 0..512),
		) {
			let limits = DecodeLimits::new(16, 256, 256, 256, 16 * 1024);
			let _result = CanonicalCodec::decode(&bytes, limits);
		}
	}

	/// Returns assert status for this module.
	fn assert_status(error: FerrosGrynnError, expected: &str) {
		assert_eq!(error.status().code().to_string(), expected);
	}
}
