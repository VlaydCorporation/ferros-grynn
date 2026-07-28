mod diagnostic;
mod error;
pub mod formatting;
mod outcome;
mod status;
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
