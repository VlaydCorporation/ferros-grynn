mod error;
pub mod params;
mod status;
mod diagnostic;

use magic_utils::{EnumPartialEq, IntoVariants};
use serde::{Deserialize, Serialize};

pub use status::{Condition, Status};
pub use error::{FerrosGrynnError, FerrosGrynnResult, FerrosGrynnErrorBuilder};

#[derive(Debug, Serialize, Deserialize, EnumPartialEq, Eq, IntoVariants, Copy, Clone)]
pub enum GqlClassification {
	NotificationClassification(NotificationClassification),
	ErrorClassification(ErrorClassification),
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Copy, Clone)]
pub enum NotificationClassification {
	/// Deprecated feature/format/functionality
	Deprecation,
	/// Unfulfillable hint warnings
	Hint,
	/// Informational notes which suggests improvements to increase performance by making changes to query/schema
	Performance,
	/// Warnings/info that are not part of a wider class
	Generic,
	/// The query or command mentions entities that are unknown to the system
	Unrecognised,
	/// Classification is unknown
	Unknown,
	/// Unsupported feature warnings
	Unsupported,
	/// Security warnings
	Security,
	/// Topology warnings and information
	Topology,
	/// Schema warnings and information
	Schema,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Copy, Clone)]
pub enum ErrorClassification {
	/// The Client sent a bad request - changing the request might yield a successful outcome.
	ClientError,
	/// The database failed to service the request.
	DatabaseError,
	/// The database cannot service the request right now, retrying later might yield a successful outcome.
	TransientError,
	/// Classification is unknown
	Unknown,
}
