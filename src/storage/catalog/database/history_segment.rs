//! Shared fixed-size authoritative references to source and computed history.

use super::codec::{Decoder, invalid};
use crate::{
	gql_status::FerrosGrynnResult,
	storage::{
		HistoryOwner,
		HistorySegment,
		history_codec::pages::SegmentHeader,
		types::DataFileId,
	},
};

/// Complete physical identity, trusted bounds, logical owner and reserved
/// bytes.
pub(super) const BYTES: usize = 128;

/// Appends a fixed admitted descriptor; the enclosing section checks capacity.
pub(super) fn encode(output: &mut Vec<u8>, segment: HistorySegment) {
	output.extend_from_slice(&segment.file.get().to_le_bytes());
	let header = segment.header;
	output.extend_from_slice(header.database.as_bytes());
	output.extend_from_slice(header.owner.uuid().as_bytes());
	output.extend_from_slice(header.segment.as_bytes());
	for value in [header.pages, header.commits, header.first_stamp, header.last_stamp] {
		output.extend_from_slice(&value.to_le_bytes());
	}
	output.extend_from_slice(&header.first_time.to_le_bytes());
	output.extend_from_slice(&header.last_time.to_le_bytes());
	output.push(header.owner.tag());
	output.extend_from_slice(&[0; 7]);
}

/// Checks the owner namespace and complete bounds before catalog publication.
pub(super) fn decode(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<HistorySegment> {
	let file = DataFileId::from_raw(decoder.read_u64()?)
		.ok_or_else(|| invalid("history file identity is invalid"))?;
	let database = uuid(decoder)?;
	let owner_id = uuid(decoder)?;
	let segment = uuid(decoder)?;
	let pages = decoder.read_u64()?;
	let commits = decoder.read_u64()?;
	let first_stamp = decoder.read_u64()?;
	let last_stamp = decoder.read_u64()?;
	let first_time = time(decoder)?;
	let last_time = time(decoder)?;
	let owner = HistoryOwner::decode(decoder.read_u8()?, owner_id)?;
	decoder.expect_zero(7, "history segment reserved bytes")?;
	let header = SegmentHeader {
		database,
		owner,
		segment,
		pages,
		commits,
		first_stamp,
		last_stamp,
		first_time,
		last_time,
	};
	header.validate()?;
	Ok(HistorySegment {
		file,
		header,
	})
}

/// Rejects nil identities before interpreting their logical namespace.
fn uuid(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<uuid::Uuid> {
	let value = uuid::Uuid::from_slice(decoder.read_exact(16)?)
		.map_err(|_error| invalid("history segment UUID is malformed"))?;
	if value.is_nil() {
		return Err(invalid("history segment UUID is nil"));
	}
	Ok(value)
}

/// Reads exact UTC epoch nanoseconds without converting to floating point.
fn time(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<i128> {
	Ok(i128::from_le_bytes(
		decoder
			.read_exact(16)?
			.try_into()
			.map_err(|_error| invalid("history segment time is truncated"))?,
	))
}
