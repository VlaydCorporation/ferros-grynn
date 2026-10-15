/// Severity, execution phases and checked source locations for diagnostics.
mod diagnostic;
/// Error-only status wrappers and structured cause chains.
mod error;
/// Formatting and capture of typed diagnostic-message parameters.
pub mod formatting;
/// Successful values accompanied by completion and notification reports.
mod outcome;
/// Typed status codes, semantic definitions and diagnostic objects.
mod status;
/// Versioned diagnostic DTOs independent of internal error storage.
mod wire;

pub use diagnostic::{
	DiagnosticRecord,
	ExecutionPhase,
	LabeledLocation,
	LineColumn,
	Location,
	LocationError,
	Severity,
	TextSpan,
};
pub use error::{FerrosGrynnError, FerrosGrynnResult, NonErrorStatus};
pub use outcome::{Outcome, StatusReport, StatusReportError};
pub use status::{
	Condition,
	Domain,
	ErrorCategory,
	ErrorClassification,
	InvalidSeverity,
	NotificationClassification,
	Status,
	StatusCode,
	StatusCodeParseError,
	StatusDefinition,
	StatusKind,
	StatusObject,
};
pub use wire::{
	DiagnosticRecordWireV1,
	LabeledLocationWireV1,
	LocationWireV1,
	STATUS_WIRE_VERSION,
	StatusWireV1,
};

/// A compatible facade for shared project statuses, diagnostics, and errors.
pub mod gql_status {
	pub use super::*;
}
