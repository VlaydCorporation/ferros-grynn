use rkyv::util::AlignedVec;

use super::{ARCHIVE_FORMAT_VERSION, ARCHIVE_HEADER_LEN, ARCHIVE_MAGIC, ARCHIVE_RESERVED_LEN};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

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

pub(super) fn crc32(bytes: &[u8]) -> u32 {
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
