//! Fixed framing for acknowledged whole-commit positions.

use super::{LiveId, LiveVersion};
use crate::{
	changefeed::Versionstamp,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Bytes, Uuid},
};

/// Owned confirmed watermark, bound to a database and query definition.
///
/// ```
/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
/// use ferros_grynn::{
/// 	changefeed::Versionstamp,
/// 	live::{LiveCursor, LiveId, LiveVersion},
/// 	values::Uuid,
/// };
/// let id = LiveId::from_uuid(Uuid::now_v7())?;
/// let cursor =
/// 	LiveCursor::new(Uuid::now_v7(), id, LiveVersion::default(), Versionstamp::new(u64::MAX))?;
/// assert_eq!(LiveCursor::decode(cursor.encode().as_slice())?, cursor);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(
	clippy::tabs_in_doc_comments,
	reason = "nightly rustfmt uses required hard tabs in Rustdoc examples"
)]
pub struct LiveCursor {
	/// Durable database UUID, independent of file generations.
	database: Uuid,
	/// Subscription incarnation, replaced on reregistration.
	query: LiveId,
	/// Definition version used to compute the retained results.
	version: LiveVersion,
	/// Last acknowledged complete commit; never a position within a commit.
	stamp: Versionstamp,
}

impl LiveCursor {
	/// Constructs an origin-bound whole-commit cursor.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for a nil database UUID.
	pub fn new(
		database: Uuid,
		query: LiveId,
		version: LiveVersion,
		stamp: Versionstamp,
	) -> FerrosGrynnResult<Self> {
		if database.as_inner().is_nil() {
			return Err(FerrosGrynnError::invalid_argument("LIVE cursor database must not be nil"));
		}
		Ok(Self {
			database,
			query,
			version,
			stamp,
		})
	}

	/// Returns the durable database origin.
	pub const fn database_id(&self) -> Uuid {
		self.database
	}

	/// Returns the subscription incarnation.
	pub const fn live_id(&self) -> LiveId {
		self.query
	}

	/// Returns the immutable query definition version.
	pub const fn version(&self) -> LiveVersion {
		self.version
	}

	/// Returns the last acknowledged whole-commit position.
	pub const fn versionstamp(&self) -> Versionstamp {
		self.stamp
	}

	/// Checks the expected database, query incarnation and definition version.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for a cursor from another origin,
	/// query or version. This does not establish that a delivery was issued.
	pub fn validate(
		&self,
		database: Uuid,
		query: LiveId,
		version: LiveVersion,
	) -> FerrosGrynnResult<()> {
		if (self.database, self.query, self.version) != (database, query, version) {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE cursor origin/query/version mismatch",
			));
		}
		Ok(())
	}

	/// Encodes the exact 56-byte version-one envelope in little-endian order.
	pub fn encode(&self) -> Bytes {
		let mut bytes = [0_u8; 56];
		bytes[..4].copy_from_slice(b"FGLC");
		bytes[4..6].copy_from_slice(&1_u16.to_le_bytes());
		// Bytes 6 and 7 remain reserved zeros, checked before identities are
		// decoded.
		bytes[8..24].copy_from_slice(self.database.as_inner().as_bytes());
		bytes[24..40].copy_from_slice(self.query.as_uuid().as_inner().as_bytes());
		bytes[40..48].copy_from_slice(&self.version.get().to_le_bytes());
		bytes[48..56].copy_from_slice(&self.stamp.get().to_le_bytes());
		Bytes::from(bytes)
	}

	/// Decodes an exact frame, without allocation driven by input lengths.
	///
	/// # Errors
	///
	/// Rejects truncation, trailing bytes, magic/version/reserved bytes, nil
	/// identities and a zero definition version.
	pub fn decode(input: &[u8]) -> FerrosGrynnResult<Self> {
		let invalid = || FerrosGrynnError::invalid_argument("malformed LIVE cursor");
		if input.len() != 56 || &input[..4] != b"FGLC" || input[4..8] != [1, 0, 0, 0] {
			return Err(invalid());
		}
		let database = uuid::Uuid::from_slice(&input[8..24]).map_err(|_error| invalid())?.into();
		let query = LiveId::from_uuid(
			uuid::Uuid::from_slice(&input[24..40]).map_err(|_error| invalid())?.into(),
		)?;
		let version = u64::from_le_bytes(input[40..48].try_into().map_err(|_error| invalid())?);
		let stamp = u64::from_le_bytes(input[48..56].try_into().map_err(|_error| invalid())?);
		Self::new(database, query, LiveVersion::new(version)?, Versionstamp::new(stamp))
	}
}

#[cfg(test)]
/// Regression tests for cursor behavior.
mod tests {
	use super::*;
	use crate::live::LiveBindingId;

	/// Verifies whole commit cursor golden and malformed frames.
	#[test]
	fn whole_commit_cursor_golden_and_malformed_frames() {
		use crate::test_assertions::AssertResult as _;

		let id = LiveId::from_uuid(uuid::Uuid::from_bytes([2; 16]).into()).assert_ok();
		let database = uuid::Uuid::from_bytes([1; 16]).into();
		for position in [0, (1_u64 << 53) + 1, u64::MAX] {
			let cursor =
				LiveCursor::new(database, id, LiveVersion::new(7).assert_ok(), position.into())
					.assert_ok();
			let bytes = cursor.encode();
			assert_eq!(&bytes.as_slice()[..8], b"FGLC\x01\x00\x00\x00");
			assert_eq!(&bytes.as_slice()[8..24], &[1; 16]);
			assert_eq!(&bytes.as_slice()[24..40], &[2; 16]);
			assert_eq!(&bytes.as_slice()[40..48], &7_u64.to_le_bytes());
			assert_eq!(&bytes.as_slice()[48..], &position.to_le_bytes());
			assert_eq!(LiveCursor::decode(bytes.as_slice()).assert_ok(), cursor);
			for length in 0..56 {
				assert!(LiveCursor::decode(&bytes.as_slice()[..length]).is_err());
			}
			for offset in 0..8 {
				let mut bad = bytes.clone().into_vec();
				bad[offset] ^= 0xff;
				assert!(LiveCursor::decode(&bad).is_err());
			}
			for range in [8..24, 24..40, 40..48] {
				let mut bad = bytes.clone().into_vec();
				bad[range].fill(0);
				assert!(LiveCursor::decode(&bad).is_err());
			}
			let mut bad = bytes.into_vec();
			bad.push(0);
			assert!(LiveCursor::decode(&bad).is_err());
			cursor.validate(database, id, LiveVersion::new(7).assert_ok()).assert_ok();
			assert!(cursor.validate(Uuid::now_v7(), id, LiveVersion::new(7).assert_ok()).is_err());
			assert!(
				cursor
					.validate(
						database,
						LiveId::from_uuid(Uuid::now_v7()).assert_ok(),
						LiveVersion::new(7).assert_ok()
					)
					.is_err()
			);
			assert!(cursor.validate(database, id, LiveVersion::new(8).assert_ok()).is_err());
		}
		assert!(LiveBindingId::from_uuid(Uuid::default()).is_err());
	}
}
