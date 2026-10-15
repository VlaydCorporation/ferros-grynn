use std::{
	fmt::{self, Display, Formatter},
	str::FromStr,
};

use ulid::Ulid as InnerUlid;
use uuid::Uuid as InnerUuid;

/// Owned UUID value with the project value, ordering and formatting interfaces.
#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct Uuid(
	/// Underlying UUID value.
	InnerUuid,
);

impl Uuid {
	/// Returns new v4 for `Uuid`.
	pub fn new_v4() -> Self {
		Self(InnerUuid::new_v4())
	}

	/// Returns now v7 for `Uuid`.
	pub fn now_v7() -> Self {
		Self(InnerUuid::now_v7())
	}

	/// Borrows the wrapped inner representation.
	pub const fn as_inner(&self) -> &InnerUuid {
		&self.0
	}

	/// Returns the wrapped inner representation.
	pub const fn into_inner(self) -> InnerUuid {
		self.0
	}
}

impl From<InnerUuid> for Uuid {
	/// Converts `InnerUuid` into `Uuid`.
	fn from(value: InnerUuid) -> Self {
		Self(value)
	}
}

impl From<Uuid> for InnerUuid {
	/// Converts `Uuid` into `InnerUuid`.
	fn from(value: Uuid) -> Self {
		value.0
	}
}

impl FromStr for Uuid {
	type Err = uuid::Error;

	/// Constructs from str for `Uuid`.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		value.parse().map(Self)
	}
}

impl Display for Uuid {
	/// Formats `Uuid` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, f)
	}
}

/// Owned ULID value with the project value, ordering and formatting interfaces.
#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct Ulid(
	/// Underlying ULID value.
	InnerUlid,
);

impl Ulid {
	/// Constructs `Ulid` from the supplied initial state.
	pub fn new() -> Self {
		Self(InnerUlid::generate())
	}

	/// Borrows the wrapped inner representation.
	pub const fn as_inner(&self) -> &InnerUlid {
		&self.0
	}

	/// Returns the wrapped inner representation.
	pub const fn into_inner(self) -> InnerUlid {
		self.0
	}
}

impl From<InnerUlid> for Ulid {
	/// Converts `InnerUlid` into `Ulid`.
	fn from(value: InnerUlid) -> Self {
		Self(value)
	}
}

impl From<Ulid> for InnerUlid {
	/// Converts `Ulid` into `InnerUlid`.
	fn from(value: Ulid) -> Self {
		value.0
	}
}

impl FromStr for Ulid {
	type Err = ulid::DecodeError;

	/// Constructs from str for `Ulid`.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		value.parse().map(Self)
	}
}

impl Display for Ulid {
	/// Formats `Ulid` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, f)
	}
}

#[cfg(test)]
/// Regression tests for id behavior.
mod tests {
	use super::{Ulid, Uuid};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies uuid round trips through text and inner type.
	#[test]
	fn uuid_round_trips_through_text_and_inner_type() {
		let value = Uuid::new_v4();
		assert_eq!(value.to_string().parse::<Uuid>().assert_ok(), value);
		assert_eq!(Uuid::from(value.into_inner()), value);
	}

	/// Verifies ulid round trips through text and inner type.
	#[test]
	fn ulid_round_trips_through_text_and_inner_type() {
		let value = Ulid::new();
		assert_eq!(value.to_string().parse::<Ulid>().assert_ok(), value);
		assert_eq!(Ulid::from(value.into_inner()), value);
	}
}
