//! Distinct subscription, reactive binding and definition version identities.

use std::{fmt, num::NonZeroU64, str::FromStr};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Uuid,
};

/// Stable subscription incarnation, independent of session and source names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LiveId(
	/// Non-nil UUID identifying one subscription incarnation.
	Uuid,
);

impl LiveId {
	/// Accepts a non-nil UUID without allocating a registry entry.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for a nil identity.
	pub fn from_uuid(value: Uuid) -> FerrosGrynnResult<Self> {
		if value.as_inner().is_nil() {
			return Err(FerrosGrynnError::invalid_argument("LIVE identity must not be nil"));
		}
		Ok(Self(value))
	}

	/// Returns the UUID used by GQL registration and control operations.
	pub const fn as_uuid(self) -> Uuid {
		self.0
	}
}

impl TryFrom<Uuid> for LiveId {
	type Error = FerrosGrynnError;

	/// Converts `Uuid` into `LiveId` after validating the conversion.
	fn try_from(value: Uuid) -> FerrosGrynnResult<Self> {
		Self::from_uuid(value)
	}
}

impl FromStr for LiveId {
	type Err = FerrosGrynnError;

	/// Constructs from str for `LiveId`.
	fn from_str(value: &str) -> FerrosGrynnResult<Self> {
		Self::from_uuid(
			value
				.parse::<Uuid>()
				.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?,
		)
	}
}

impl fmt::Display for LiveId {
	/// Formats `LiveId` using the requested formatter.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.0, f)
	}
}

/// Stable reactive binding identity; lexical shadowing does not rebind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LiveBindingId(
	/// Non-nil UUID identifying one reactive binding independently of its
	/// lexical name.
	Uuid,
);

impl LiveBindingId {
	/// Accepts a non-nil binding UUID, independently of any lexical name.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for a nil identity.
	pub fn from_uuid(value: Uuid) -> FerrosGrynnResult<Self> {
		if value.as_inner().is_nil() {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE binding identity must not be nil",
			));
		}
		Ok(Self(value))
	}

	/// Returns the owned public UUID.
	pub const fn as_uuid(self) -> Uuid {
		self.0
	}
}

impl TryFrom<Uuid> for LiveBindingId {
	type Error = FerrosGrynnError;

	/// Converts `Uuid` into `LiveBindingId` after validating the conversion.
	fn try_from(value: Uuid) -> FerrosGrynnResult<Self> {
		Self::from_uuid(value)
	}
}

impl FromStr for LiveBindingId {
	type Err = FerrosGrynnError;

	/// Constructs from str for `LiveBindingId`.
	fn from_str(value: &str) -> FerrosGrynnResult<Self> {
		Self::from_uuid(
			value
				.parse::<Uuid>()
				.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?,
		)
	}
}

impl fmt::Display for LiveBindingId {
	/// Formats `LiveBindingId` using the requested formatter.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.0, f)
	}
}

/// Nonzero version of a query or binding definition, separate from commit time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LiveVersion(
	/// Positive version distinguishing successive query or binding definitions.
	NonZeroU64,
);

impl LiveVersion {
	/// Checks a definition version read from a catalog or cursor.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for zero.
	pub fn new(value: u64) -> FerrosGrynnResult<Self> {
		NonZeroU64::new(value)
			.map(Self)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE version must be positive"))
	}

	/// Returns the exact version, without a floating-point conversion.
	pub const fn get(self) -> u64 {
		self.0.get()
	}
}

impl Default for LiveVersion {
	/// Constructs the default initial state of `LiveVersion`.
	fn default() -> Self {
		Self(NonZeroU64::MIN)
	}
}
