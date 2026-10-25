//! Explicit retained-source bytes; no primary records are needed to decode
//! them.

use fg_runtime::__internal::DurationInternal as _;

use super::{
	ChangefeedCatalogEntry,
	codec::{Decoder, invalid},
};
use crate::{
	changefeed::{
		ChangefeedCaptureEpoch,
		ChangefeedConfig,
		ChangefeedId,
		ChangefeedSource,
		ChangefeedState,
	},
	datastore::{ClassId, SchemaVersion},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
};

/// Minimum descriptor bytes before its name and capture epochs.
pub(super) const MIN_BYTES: usize = 152;
/// Complete physical identity, trusted bounds and canonical reserved bytes.
const SEGMENT_BYTES: usize = super::history_segment::BYTES;
/// Shared immutable catalog payload cap.
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
/// Maximum owned UTF-8 source name.
const MAX_NAME_BYTES: usize = 4096;

/// Checks the complete descriptor size before extending the output buffer.
pub(super) fn encode(
	output: &mut Vec<u8>,
	entry: &ChangefeedCatalogEntry,
) -> FerrosGrynnResult<()> {
	let name = entry.name.as_str().as_bytes();
	if name.is_empty() || name.len() > MAX_NAME_BYTES {
		return Err(invalid("changefeed source name has an invalid length"));
	}
	let required = output
		.len()
		.checked_add(MIN_BYTES)
		.and_then(|size| size.checked_add(name.len()))
		.and_then(|size| size.checked_add(entry.epochs.len().checked_mul(32)?))
		.and_then(|size| size.checked_add(entry.segments.len().checked_mul(SEGMENT_BYTES)?))
		.ok_or_else(|| invalid("changefeed descriptor length overflows"))?;
	if required > MAX_CATALOG_BYTES {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"database catalog bytes",
			required as u64,
			MAX_CATALOG_BYTES as u64,
		));
	}
	let (source_kind, source) = match entry.source {
		ChangefeedSource::Class(id) => (0, u64::from(id.get())),
		ChangefeedSource::Graph(id) => (1, id.get()),
	};
	let state = match entry.state {
		ChangefeedState::Active => 0,
		ChangefeedState::Stopped => 1,
		ChangefeedState::Retired => 2,
	};
	let flags = u8::from(entry.config.include_original()) | (u8::from(entry.config.full()) << 1);
	output.extend_from_slice(entry.id.as_uuid().as_inner().as_bytes());
	output.extend_from_slice(&[source_kind, state, flags, u8::from(entry.closed_at.is_some())]);
	output.extend_from_slice(&source.to_le_bytes());
	output.extend_from_slice(&entry.version.get().to_le_bytes());
	output.extend_from_slice(&0_u32.to_le_bytes());
	let retention = entry
		.config
		.retention()
		.fixed_nanoseconds()
		.ok_or_else(|| invalid("changefeed retention is not a fixed duration"))?;
	output.extend_from_slice(&retention.to_le_bytes());
	for stamp in [
		entry.defined_at,
		entry.published_at,
		entry.last_stamp,
		entry.purged_through,
		entry.closed_at.unwrap_or(0),
	] {
		output.extend_from_slice(&stamp.to_le_bytes());
	}
	let name_len =
		u32::try_from(name.len()).map_err(|_error| invalid("source name length overflows"))?;
	output.extend_from_slice(&entry.purged_time_ns.to_le_bytes());
	output.extend_from_slice(&entry.closed_time_ns.unwrap_or(0).to_le_bytes());
	output.extend_from_slice(&entry.retained_page.to_le_bytes());
	output.extend_from_slice(&entry.retained_groups.to_le_bytes());
	output.extend_from_slice(&name_len.to_le_bytes());
	output.extend_from_slice(name);
	let count = u32::try_from(entry.epochs.len())
		.map_err(|_error| invalid("capture epoch count overflows"))?;
	output.extend_from_slice(&count.to_le_bytes());
	for epoch in &entry.epochs {
		output.extend_from_slice(&epoch.started_at().get().to_le_bytes());
		let (flags, nanos) = if let Some(config) = epoch.configuration() {
			let nanos = config
				.retention()
				.fixed_nanoseconds()
				.ok_or_else(|| invalid("capture epoch retention is not fixed"))?;
			(1 | (u8::from(config.full()) << 1) | (u8::from(config.include_original()) << 2), nanos)
		} else {
			(0, 0)
		};
		output.push(flags);
		output.extend_from_slice(&[0; 3]);
		output.extend_from_slice(&epoch.revision().get().to_le_bytes());
		output.extend_from_slice(&nanos.to_le_bytes());
	}
	let count = u32::try_from(entry.segments.len())
		.map_err(|_error| invalid("feed segment count overflows"))?;
	output.extend_from_slice(&count.to_le_bytes());
	for segment in &entry.segments {
		super::history_segment::encode(output, *segment);
	}
	Ok(())
}

/// Validates tags, reserved bytes, durations and lengths before allocation.
pub(super) fn decode(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<ChangefeedCatalogEntry> {
	let id = ChangefeedId::from_uuid(
		uuid::Uuid::from_slice(decoder.read_exact(16)?)
			.map_err(|_error| invalid("changefeed UUID is malformed"))?
			.into(),
	)
	.map_err(|_error| invalid("changefeed UUID is nil"))?;
	let source_kind = decoder.read_u8()?;
	let state = match decoder.read_u8()? {
		0 => ChangefeedState::Active,
		1 => ChangefeedState::Stopped,
		2 => ChangefeedState::Retired,
		_ => return Err(invalid("unknown changefeed lifecycle")),
	};
	let flags = decoder.read_u8()?;
	if flags & !3 != 0 {
		return Err(invalid("unknown changefeed capture flags"));
	}
	let closed = match decoder.read_u8()? {
		0 => false,
		1 => true,
		_ => return Err(invalid("invalid close-stamp presence")),
	};
	let source_id = decoder.read_u64()?;
	let source = match source_kind {
		0 => ChangefeedSource::Class(ClassId::from_storage(
			u32::try_from(source_id)
				.map_err(|_error| invalid("class source identity overflows"))?,
		)?),
		1 => ChangefeedSource::Graph(
			GraphId::new(source_id).ok_or_else(|| invalid("graph source identity is zero"))?,
		),
		_ => return Err(invalid("unknown changefeed source kind")),
	};
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	decoder.expect_zero(4, "changefeed descriptor reserved bytes")?;
	let retention = i128::from_le_bytes(
		decoder
			.read_exact(16)?
			.try_into()
			.map_err(|_error| invalid("changefeed retention is truncated"))?,
	);
	let config = ChangefeedConfig::from_nanoseconds(retention)
		.map_err(|_error| invalid("invalid fixed changefeed retention"))?
		.with_include_original(flags & 1 != 0)
		.with_full(flags & 2 != 0);
	let defined_at = decoder.read_u64()?;
	let published_at = decoder.read_u64()?;
	let last_stamp = decoder.read_u64()?;
	let purged_through = decoder.read_u64()?;
	let closed_stamp = decoder.read_u64()?;
	if !closed && closed_stamp != 0 {
		return Err(invalid("absent close stamp is non-zero"));
	}
	let purged_time_ns = i128::from_le_bytes(
		decoder
			.read_exact(16)?
			.try_into()
			.map_err(|_error| invalid("changefeed purge time is truncated"))?,
	);
	let closed_time_ns = read_i128(decoder)?;
	if !closed && closed_time_ns != 0 {
		return Err(invalid("absent close time is non-zero"));
	}
	let retained_page = decoder.read_u64()?;
	let retained_groups = decoder.read_u64()?;
	let length = decoder.read_count("changefeed source name bytes")?;
	if length == 0 || length > MAX_NAME_BYTES {
		return Err(invalid("invalid changefeed source name length"));
	}
	let name = std::str::from_utf8(decoder.read_exact(length)?)
		.map_err(|_error| invalid("changefeed source name is not UTF-8"))?
		.into();
	let count = decoder.read_count("capture epoch count")?;
	let remaining = decoder.remaining_bytes();
	if count == 0 || count > remaining / 32 {
		return Err(invalid("capture epoch count exceeds remaining bytes"));
	}
	let mut epochs = Vec::with_capacity(count);
	for _ in 0..count {
		let started_at = decoder.read_u64()?.into();
		let flags = decoder.read_u8()?;
		if flags & !7 != 0 || flags & 1 == 0 && flags != 0 {
			return Err(invalid("capture epoch flags are invalid"));
		}
		decoder.expect_zero(3, "capture epoch reserved bytes")?;
		let revision = SchemaVersion::new(decoder.read_u32()?)
			.ok_or_else(|| invalid("capture epoch revision is reserved"))?;
		let nanos = i128::from_le_bytes(
			decoder
				.read_exact(16)?
				.try_into()
				.map_err(|_error| invalid("capture epoch duration is truncated"))?,
		);
		let configuration = if flags & 1 != 0 {
			Some(
				ChangefeedConfig::from_nanoseconds(nanos)
					.map_err(|_error| invalid("capture epoch duration is invalid"))?
					.with_full(flags & 2 != 0)
					.with_include_original(flags & 4 != 0),
			)
		} else {
			if nanos != 0 {
				return Err(invalid("disabled capture epoch has a retention"));
			}
			None
		};
		epochs.push(ChangefeedCaptureEpoch {
			started_at,
			revision,
			configuration,
		});
	}
	let count = decoder.read_count("feed segment count")?;
	if count > decoder.remaining_bytes() / SEGMENT_BYTES {
		return Err(invalid("feed segment count exceeds remaining catalog bytes"));
	}
	let mut segments = Vec::with_capacity(count);
	for _ in 0..count {
		segments.push(super::history_segment::decode(decoder)?);
	}
	Ok(ChangefeedCatalogEntry {
		id,
		source,
		name,
		config,
		state,
		version,
		defined_at,
		published_at,
		last_stamp,
		purged_through,
		purged_time_ns,
		retained_page,
		retained_groups,
		closed_at: closed.then_some(closed_stamp),
		closed_time_ns: closed.then_some(closed_time_ns),
		epochs,
		segments,
	})
}

/// Reads an exact signed UTC nanosecond field without unchecked indexing.
fn read_i128(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<i128> {
	Ok(i128::from_le_bytes(
		decoder
			.read_exact(16)?
			.try_into()
			.map_err(|_error| invalid("segment time is truncated"))?,
	))
}
