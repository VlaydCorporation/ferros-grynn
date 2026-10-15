use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::gql_status::{
	diagnostic::{ExecutionPhase, LabeledLocation, Location, Severity},
	error::FerrosGrynnError,
	formatting::DiagnosticValue,
	outcome::StatusReport,
	status::{
		Condition,
		Domain,
		ErrorCategory,
		ErrorClassification,
		NotificationClassification,
		StatusKind,
		StatusObject,
	},
};

/// Version tag accepted and emitted by the status wire DTOs.
pub const STATUS_WIRE_VERSION: u16 = 1;

/// A versioned representation of a status and its causes for transmission or
/// serialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StatusWireV1 {
	/// The version of the status wire representation.
	pub version: u16,
	/// The stable code that determines the status kind.
	pub code: String,
	/// A human-readable description of the status.
	pub description: String,
	/// An optional message with substituted typed parameters.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub message: Option<String>,
	/// Named status parameters in a structured representation.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub parameters: BTreeMap<String, DiagnosticValue>,
	/// The severity specified by the status kind or an invalid override.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub severity: Option<Severity>,
	/// The notification or error classification for client handling.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub classification: Option<String>,
	/// The semantic error category, if applicable.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub category: Option<ErrorCategory>,
	/// The standard completion condition that determines the code class.
	pub condition: Condition,
	/// The subsystem responsible for the status.
	pub domain: Domain,
	/// The wire representation of the diagnostic-event context.
	pub diagnostic_record: DiagnosticRecordWireV1,
	/// The immediate structured cause of the error.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cause: Option<Box<StatusWireV1>>,
}

/// A versioned representation of the diagnostic-event context.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DiagnosticRecordWireV1 {
	/// The optional primary diagnostic location.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub position: Option<LocationWireV1>,
	/// Additional diagnostic locations with explanatory labels.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub related: Vec<LabeledLocationWireV1>,
	/// The optional timestamp of the diagnostic event.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub timestamp: Option<String>,
	/// The optional processing phase in which the event occurred.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub phase: Option<ExecutionPhase>,
	/// Additional structured data for the diagnostic event.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub extensions: BTreeMap<String, DiagnosticValue>,
}

/// The wire representation of a source-text range and its boundary positions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LocationWireV1 {
	/// The optional logical name of the source text.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub source_id: Option<String>,
	/// The starting byte offset in the UTF-8 source text.
	pub offset: u32,
	/// The length of the transmitted range in bytes.
	pub length: u32,
	/// The one-based starting line number.
	pub line: u32,
	/// The one-based starting column number.
	pub column: u32,
	/// The line number immediately after the end of the range.
	pub end_line: u32,
	/// The column number immediately after the end of the range.
	pub end_column: u32,
}

/// The wire representation of a diagnostic location with an optional
/// explanatory label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LabeledLocationWireV1 {
	/// The source-text range explained by this label.
	#[serde(flatten)]
	pub location: LocationWireV1,
	/// The optional explanatory label for the location.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub label: Option<String>,
}

impl StatusObject {
	/// Creates a versioned representation for transmission or serialization.
	pub fn to_wire(&self) -> StatusWireV1 {
		status_to_wire(self, None)
	}
}

impl FerrosGrynnError {
	/// Creates a versioned representation for transmission or serialization.
	pub fn to_wire(&self) -> StatusWireV1 {
		status_to_wire(self.status(), self.cause().map(FerrosGrynnError::to_wire).map(Box::new))
	}
}

impl StatusReport {
	/// Creates a versioned representation for transmission or serialization.
	pub fn to_wire(&self) -> Vec<StatusWireV1> {
		self.iter().map(StatusObject::to_wire).collect()
	}
}

/// Converts the structured status and its parameters to the wire
/// representation.
fn status_to_wire(status: &StatusObject, cause: Option<Box<StatusWireV1>>) -> StatusWireV1 {
	let definition = status.definition();
	let (classification, category) = match definition.kind {
		StatusKind::Completion => (None, None),
		StatusKind::Notification {
			classification,
			..
		} => (Some(notification_classification(classification).to_owned()), None),
		StatusKind::Error {
			classification,
			category,
			..
		} => (Some(error_classification(classification).to_owned()), Some(category)),
	};
	StatusWireV1 {
		version: STATUS_WIRE_VERSION,
		code: status.code().to_string(),
		description: status.description(),
		message: status.message(),
		parameters: status
			.parameters()
			.into_iter()
			.map(|(key, value)| (key.to_owned(), value))
			.collect(),
		severity: status.effective_severity(),
		classification,
		category,
		condition: definition.condition,
		domain: definition.domain,
		diagnostic_record: DiagnosticRecordWireV1 {
			position: status.diagnostic().primary_location().map(location_to_wire),
			related: status
				.diagnostic()
				.related_locations()
				.iter()
				.map(labeled_location_to_wire)
				.collect(),
			timestamp: status
				.diagnostic()
				.timestamp()
				.map(|timestamp| humantime::format_rfc3339(timestamp).to_string()),
			phase: status.diagnostic().phase(),
			extensions: status.diagnostic().extensions().clone(),
		},
		cause,
	}
}

/// Converts a diagnostic location to the wire representation.
fn location_to_wire(location: &Location) -> LocationWireV1 {
	LocationWireV1 {
		source_id: location.source_id().map(str::to_owned),
		offset: location.span().offset,
		length: location.span().len,
		line: location.start().line.get(),
		column: location.start().column.get(),
		end_line: location.end().line.get(),
		end_column: location.end().column.get(),
	}
}

/// Converts a location with an explanatory label to the wire representation.
fn labeled_location_to_wire(location: &LabeledLocation) -> LabeledLocationWireV1 {
	LabeledLocationWireV1 {
		location: location_to_wire(location.location()),
		label: location.label().map(str::to_owned),
	}
}

/// Returns the error classification if the status describes an error.
const fn error_classification(classification: ErrorClassification) -> &'static str {
	match classification {
		ErrorClassification::ClientError => "CLIENT_ERROR",
		ErrorClassification::DatabaseError => "DATABASE_ERROR",
		ErrorClassification::TransientError => "TRANSIENT_ERROR",
	}
}

/// Returns the notification classification if the status describes a
/// notification.
const fn notification_classification(classification: NotificationClassification) -> &'static str {
	match classification {
		NotificationClassification::Deprecation => "DEPRECATION",
		NotificationClassification::Hint => "HINT",
		NotificationClassification::Performance => "PERFORMANCE",
		NotificationClassification::Generic => "GENERIC",
		NotificationClassification::Unrecognised => "UNRECOGNISED",
		NotificationClassification::Unsupported => "UNSUPPORTED",
		NotificationClassification::Security => "SECURITY",
		NotificationClassification::Topology => "TOPOLOGY",
		NotificationClassification::Schema => "SCHEMA",
	}
}

#[cfg(test)]
/// Regression tests for wire behavior.
mod tests {
	use super::*;
	use crate::gql_status::Status;

	/// Tests the `serializes_only_structured_causes` scenario and its
	/// diagnostic outcome.
	#[test]
	fn serializes_only_structured_causes() {
		let error = FerrosGrynnError::struct_build_error("Example")
			.with_cause(FerrosGrynnError::unknown_field("missing"));
		let wire = error.to_wire();

		assert_eq!(wire.version, 1);
		assert_eq!(wire.code, "FG-22F01");
		assert_eq!(wire.classification.as_deref(), Some("CLIENT_ERROR"));
		assert_eq!(wire.cause.unwrap().code, "FG-22F02");
	}

	/// Tests the `completion_omits_severity_and_classification` scenario and
	/// its diagnostic outcome.
	#[test]
	fn completion_omits_severity_and_classification() {
		let wire = StatusObject::new(Status::successful_completion()).to_wire();
		assert_eq!(wire.severity, None);
		assert_eq!(wire.classification, None);
	}
}
