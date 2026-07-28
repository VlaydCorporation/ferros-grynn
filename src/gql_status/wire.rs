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

pub const STATUS_WIRE_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusWireV1 {
	pub version: u16,
	pub code: String,
	pub description: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub message: Option<String>,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub parameters: BTreeMap<String, DiagnosticValue>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub severity: Option<Severity>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub classification: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub category: Option<ErrorCategory>,
	pub condition: Condition,
	pub domain: Domain,
	pub diagnostic_record: DiagnosticRecordWireV1,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cause: Option<Box<StatusWireV1>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticRecordWireV1 {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub position: Option<LocationWireV1>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub related: Vec<LabeledLocationWireV1>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub timestamp: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub phase: Option<ExecutionPhase>,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub extensions: BTreeMap<String, DiagnosticValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationWireV1 {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub source_id: Option<String>,
	pub offset: u32,
	pub length: u32,
	pub line: u32,
	pub column: u32,
	pub end_line: u32,
	pub end_column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabeledLocationWireV1 {
	#[serde(flatten)]
	pub location: LocationWireV1,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub label: Option<String>,
}

impl StatusObject {
	pub fn to_wire(&self) -> StatusWireV1 {
		status_to_wire(self, None)
	}
}

impl FerrosGrynnError {
	pub fn to_wire(&self) -> StatusWireV1 {
		status_to_wire(self.status(), self.cause().map(FerrosGrynnError::to_wire).map(Box::new))
	}
}

impl StatusReport {
	pub fn to_wire(&self) -> Vec<StatusWireV1> {
		self.iter().map(StatusObject::to_wire).collect()
	}
}

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

fn labeled_location_to_wire(location: &LabeledLocation) -> LabeledLocationWireV1 {
	LabeledLocationWireV1 {
		location: location_to_wire(location.location()),
		label: location.label().map(str::to_owned),
	}
}

const fn error_classification(classification: ErrorClassification) -> &'static str {
	match classification {
		ErrorClassification::ClientError => "CLIENT_ERROR",
		ErrorClassification::DatabaseError => "DATABASE_ERROR",
		ErrorClassification::TransientError => "TRANSIENT_ERROR",
	}
}

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
mod tests {
	use super::*;
	use crate::gql_status::Status;

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

	#[test]
	fn completion_omits_severity_and_classification() {
		let wire = StatusObject::new(Status::successful_completion()).to_wire();
		assert_eq!(wire.severity, None);
		assert_eq!(wire.classification, None);
	}
}
