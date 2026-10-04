//! Owned configuration and resumable identities for durable change history.
//!
//! Capture belongs to datastore transactions; physical pages and codecs remain
//! private to storage. A cursor resumes after a complete commit.

mod batch;
mod config;
mod cursor;
mod image;
mod info;
mod observation;
mod reader;

use std::{
	fmt::{self, Display, Formatter},
	str::FromStr,
};

pub use batch::{ChangeBatch, ChangefeedGap, ChangefeedGapReason, ChangefeedReadItem};
pub use config::ChangefeedConfig;
pub use cursor::{ChangefeedCursor, ChangefeedReadMode};
pub use image::{
	ChangeEndpoint,
	ChangeEntry,
	ChangeGraphState,
	ChangeIdentity,
	ChangeImage,
	ChangeOverlay,
};
pub use info::{ChangefeedCaptureEpoch, ChangefeedInfo, ChangefeedState};
pub use observation::{ChangefeedReadBinding, ChangefeedReadStatistics, ChangefeedStatistics};
pub(crate) use reader::ReadBatches;
pub use reader::{ChangefeedReadOptions, ChangefeedReader, ChangefeedSince};

use crate::{
	datastore::ClassId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	values::{Number, Text, Uuid},
};

/// The database-wide durable commit clock, including commits without events.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Versionstamp(u64);

impl Versionstamp {
	/// Creates an exact commit position. Zero denotes the database origin.
	pub const fn new(value: u64) -> Self {
		Self(value)
	}

	/// Returns the exact integer position.
	pub const fn get(self) -> u64 {
		self.0
	}
}

impl From<u64> for Versionstamp {
	fn from(value: u64) -> Self {
		Self::new(value)
	}
}

impl From<Versionstamp> for Number {
	fn from(value: Versionstamp) -> Self {
		Self::from(value.get())
	}
}

impl Display for Versionstamp {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.get(), f)
	}
}

/// Stable identity of one retained changefeed incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChangefeedId(Uuid);

impl ChangefeedId {
	/// Creates an identity from a non-nil UUID.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error for a nil UUID.
	pub fn from_uuid(value: Uuid) -> FerrosGrynnResult<Self> {
		if value.as_inner().is_nil() {
			return Err(FerrosGrynnError::invalid_argument("changefeed identity must not be nil"));
		}
		Ok(Self(value))
	}

	/// Returns the stable UUID, independent of source names.
	pub const fn as_uuid(self) -> Uuid {
		self.0
	}

	/// Allocates a fresh identity for a new source incarnation.
	pub(crate) fn allocate() -> Self {
		Self(Uuid::now_v7())
	}
}

impl TryFrom<Uuid> for ChangefeedId {
	type Error = FerrosGrynnError;

	fn try_from(value: Uuid) -> FerrosGrynnResult<Self> {
		Self::from_uuid(value)
	}
}

impl FromStr for ChangefeedId {
	type Err = FerrosGrynnError;

	fn from_str(value: &str) -> FerrosGrynnResult<Self> {
		let value = value
			.parse::<Uuid>()
			.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?;
		Self::from_uuid(value)
	}
}

impl Display for ChangefeedId {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, f)
	}
}

/// Catalog-owned scope of one changefeed; containment does not expand it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ChangefeedSource {
	/// Database-global records of one immutable class identity.
	Class(ClassId),
	/// Qualified occurrences belonging to one graph identity.
	Graph(GraphId),
}

/// Resolves active or retained historical sources in the selected snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangefeedSelector {
	/// Selects an exact source incarnation even after its owner is removed.
	Id(ChangefeedId),
	/// Selects the current or retained definition of a stable catalog identity.
	Source(ChangefeedSource),
	/// Active class names win; retired names must be unambiguous.
	ClassName(Text),
	/// Active graph names win; retired names must be unambiguous.
	GraphName(Text),
}

impl From<ChangefeedId> for ChangefeedSelector {
	fn from(value: ChangefeedId) -> Self {
		Self::Id(value)
	}
}

impl From<ChangefeedSource> for ChangefeedSelector {
	fn from(value: ChangefeedSource) -> Self {
		Self::Source(value)
	}
}
