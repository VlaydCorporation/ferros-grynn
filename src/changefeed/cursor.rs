//! Explicit bounded cursor framing; process-local QueryCursor is unrelated.

use super::{ChangefeedId, Versionstamp};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Bytes, Uuid},
};

/// Selects a stable view of retained commit history.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChangefeedReadMode {
	/// Net changes per identity and commit.
	#[default]
	Net,
	/// Captured successful operations in canonical operation order.
	Full,
}

/// Owned durable position after one complete commit in one source and mode.
///
/// ```
/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
/// use ferros_grynn::{
/// 	changefeed::{ChangefeedCursor, ChangefeedId, ChangefeedReadMode, Versionstamp},
/// 	values::Uuid,
/// };
/// let source: ChangefeedId = "00000000-0000-0000-0000-000000000001".parse()?;
/// let cursor = ChangefeedCursor::new(
/// 	Uuid::now_v7(),
/// 	source,
/// 	ChangefeedReadMode::Net,
/// 	Versionstamp::new(u64::MAX),
/// )?;
/// assert_eq!(ChangefeedCursor::decode(cursor.encode().as_slice())?, cursor);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(
	clippy::tabs_in_doc_comments,
	reason = "nightly rustfmt uses the required hard tabs in Rustdoc examples"
)]
pub struct ChangefeedCursor {
	/// Durable database origin for cursor validation.
	database: Uuid,
	/// Stable owning class or graph identity.
	source: ChangefeedId,
	/// Immutable NET or FULL read view.
	mode: ChangefeedReadMode,
	/// Last completely consumed commit position.
	stamp: Versionstamp,
}

impl ChangefeedCursor {
	/// Constructs a cursor bound to a non-nil database identity.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error for a nil database identity. Reading
	/// separately checks the selected database, source, and mode.
	pub fn new(
		database: Uuid,
		source: ChangefeedId,
		mode: ChangefeedReadMode,
		stamp: Versionstamp,
	) -> FerrosGrynnResult<Self> {
		if database.as_inner().is_nil() {
			return Err(FerrosGrynnError::invalid_argument(
				"cursor database identity must not be nil",
			));
		}
		Ok(Self {
			database,
			source,
			mode,
			stamp,
		})
	}

	/// Returns the durable database origin.
	pub const fn database_id(&self) -> Uuid {
		self.database
	}

	/// Returns the source incarnation, independent of names.
	pub const fn source_id(&self) -> ChangefeedId {
		self.source
	}

	/// Returns the immutable read mode.
	pub const fn mode(&self) -> ChangefeedReadMode {
		self.mode
	}

	/// Returns the last completely consumed commit position.
	pub const fn versionstamp(&self) -> Versionstamp {
		self.stamp
	}

	/// Encodes the fixed 48-byte, little-endian version-one envelope.
	pub fn encode(&self) -> Bytes {
		let mut output = [0_u8; 48];
		output[..4].copy_from_slice(b"FGCF");
		output[4..6].copy_from_slice(&1_u16.to_le_bytes());
		output[6] = match self.mode {
			ChangefeedReadMode::Net => 0,
			ChangefeedReadMode::Full => 1,
		};
		// Byte 7 stays zero as a reserved format byte; decoding rejects nonzero
		// values.
		output[8..24].copy_from_slice(self.database.as_inner().as_bytes());
		output[24..40].copy_from_slice(self.source.as_uuid().as_inner().as_bytes());
		output[40..48].copy_from_slice(&self.stamp.get().to_le_bytes());
		Bytes::from(output)
	}

	/// Decodes an exact cursor frame without allocation from input lengths.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error for malformed length, magic, version,
	/// mode, reserved bytes, or nil identities. Trailing bytes are rejected.
	pub fn decode(input: &[u8]) -> FerrosGrynnResult<Self> {
		let invalid = || FerrosGrynnError::invalid_argument("malformed changefeed cursor");
		if input.len() != 48 || &input[..4] != b"FGCF" || input[4..6] != [1, 0] || input[7] != 0 {
			return Err(invalid());
		}
		let mode = match input[6] {
			0 => ChangefeedReadMode::Net,
			1 => ChangefeedReadMode::Full,
			_ => return Err(invalid()),
		};
		let database = uuid::Uuid::from_slice(&input[8..24]).map_err(|_error| invalid())?.into();
		let source = ChangefeedId::from_uuid(
			uuid::Uuid::from_slice(&input[24..40]).map_err(|_error| invalid())?.into(),
		)?;
		let stamp = u64::from_le_bytes(input[40..48].try_into().map_err(|_error| invalid())?);
		Self::new(database, source, mode, Versionstamp::new(stamp))
	}
}

#[cfg(test)]
/// Regression tests for cursor behavior.
mod tests {
	use super::*;

	/// Verifies cursor preserves exact positions and rejects malformed frames.
	#[test]
	fn cursor_preserves_exact_positions_and_rejects_malformed_frames() {
		use crate::test_assertions::AssertResult as _;

		let source = ChangefeedId::from_uuid(uuid::Uuid::from_bytes([2; 16]).into()).assert_ok();
		for stamp in [0, 1_u64 << 53, u64::MAX] {
			for mode in [ChangefeedReadMode::Net, ChangefeedReadMode::Full] {
				let cursor = ChangefeedCursor::new(
					uuid::Uuid::from_bytes([1; 16]).into(),
					source,
					mode,
					Versionstamp::new(stamp),
				)
				.assert_ok();
				let encoded = cursor.encode();
				assert_eq!(ChangefeedCursor::decode(encoded.as_slice()).assert_ok(), cursor);
				for length in 0..48 {
					assert!(ChangefeedCursor::decode(&encoded.as_slice()[..length]).is_err());
				}
				for offset in [0, 4, 6, 7] {
					let mut malformed = encoded.clone().into_vec();
					malformed[offset] = 255;
					assert!(ChangefeedCursor::decode(&malformed).is_err());
				}
				let mut trailing = encoded.into_vec();
				trailing.push(0);
				assert!(ChangefeedCursor::decode(&trailing).is_err());
			}
		}
		assert!(ChangefeedId::from_uuid(Uuid::default()).is_err());
	}
}
