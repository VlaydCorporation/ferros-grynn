use std::{
	fmt::{self, Display, Formatter},
	str::FromStr,
};

use ulid::Ulid as InnerUlid;
use uuid::Uuid as InnerUuid;

#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct Uuid(InnerUuid);

impl Uuid {
	pub fn new_v4() -> Self {
		Self(InnerUuid::new_v4())
	}

	pub fn now_v7() -> Self {
		Self(InnerUuid::now_v7())
	}

	pub fn as_inner(&self) -> &InnerUuid {
		&self.0
	}

	pub fn into_inner(self) -> InnerUuid {
		self.0
	}
}

impl From<InnerUuid> for Uuid {
	fn from(value: InnerUuid) -> Self {
		Self(value)
	}
}

impl From<Uuid> for InnerUuid {
	fn from(value: Uuid) -> Self {
		value.0
	}
}

impl FromStr for Uuid {
	type Err = uuid::Error;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		value.parse().map(Self)
	}
}

impl Display for Uuid {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, formatter)
	}
}

#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct Ulid(InnerUlid);

impl Ulid {
	pub fn new() -> Self {
		Self(InnerUlid::generate())
	}

	pub fn as_inner(&self) -> &InnerUlid {
		&self.0
	}

	pub fn into_inner(self) -> InnerUlid {
		self.0
	}
}

impl From<InnerUlid> for Ulid {
	fn from(value: InnerUlid) -> Self {
		Self(value)
	}
}

impl From<Ulid> for InnerUlid {
	fn from(value: Ulid) -> Self {
		value.0
	}
}

impl FromStr for Ulid {
	type Err = ulid::DecodeError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		value.parse().map(Self)
	}
}

impl Display for Ulid {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, formatter)
	}
}

#[cfg(test)]
mod tests {
	use super::{Ulid, Uuid};

	#[test]
	fn uuid_round_trips_through_text_and_inner_type() {
		let value = Uuid::new_v4();
		assert_eq!(value.to_string().parse::<Uuid>().unwrap(), value);
		assert_eq!(Uuid::from(value.into_inner()), value);
	}

	#[test]
	fn ulid_round_trips_through_text_and_inner_type() {
		let value = Ulid::new();
		assert_eq!(value.to_string().parse::<Ulid>().unwrap(), value);
		assert_eq!(Ulid::from(value.into_inner()), value);
	}
}
