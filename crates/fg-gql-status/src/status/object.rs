use std::{
	collections::BTreeMap,
	error::Error,
	fmt::{self, Display, Formatter},
	time::SystemTime,
};

use super::{
	Condition,
	Domain,
	ErrorCategory,
	ErrorClassification,
	NotificationClassification,
	Status,
	StatusCode,
	StatusDefinition,
};
use crate::gql_status::{
	diagnostic::{DiagnosticRecord, ExecutionPhase, LabeledLocation, Location, Severity},
	formatting::DiagnosticValue,
};

/// A typed status together with the context of a specific diagnostic
/// event.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusObject {
	/// The status whose additional context or cause is stored.
	status: Status,
	/// The context of a specific diagnostic event.
	diagnostic: DiagnosticRecord,
}

impl StatusObject {
	/// Creates a value from the supplied inputs.
	pub fn new(status: Status) -> Self {
		Self {
			status,
			diagnostic: DiagnosticRecord::default(),
		}
	}

	/// Returns the structured status of this result.
	pub const fn status(&self) -> &Status {
		&self.status
	}

	/// Returns the static descriptor for this status kind.
	pub const fn definition(&self) -> &'static StatusDefinition {
		self.status.definition()
	}

	/// Returns the stable status code.
	pub const fn code(&self) -> StatusCode {
		self.definition().code
	}

	/// Returns the completion condition to which the status belongs.
	pub const fn condition(&self) -> Condition {
		self.definition().condition
	}

	/// Returns the subsystem responsible for the status.
	pub const fn domain(&self) -> Domain {
		self.definition().domain
	}

	/// Returns the catalog-defined hint for handling the status.
	pub const fn hint(&self) -> Option<&'static str> {
		self.definition().hint
	}

	/// Returns the error classification if the status describes an error.
	pub const fn error_classification(&self) -> Option<ErrorClassification> {
		self.definition().kind.error_classification()
	}

	/// Returns the notification classification if the status describes a
	/// notification.
	pub const fn notification_classification(&self) -> Option<NotificationClassification> {
		self.definition().kind.notification_classification()
	}

	/// Returns the error category if it applies to this status.
	pub const fn error_category(&self) -> Option<ErrorCategory> {
		self.definition().kind.error_category()
	}

	/// Returns the data for the specific diagnostic event.
	pub const fn diagnostic(&self) -> &DiagnosticRecord {
		&self.diagnostic
	}

	/// Returns the named message parameters in structured form.
	pub fn parameters(&self) -> BTreeMap<&'static str, DiagnosticValue> {
		self.status.parameters()
	}

	/// Formats the status message with its typed parameters.
	pub fn message(&self) -> Option<String> {
		self.status.message()
	}

	/// Returns a human-readable description of the status or condition.
	pub fn description(&self) -> String {
		let definition = self.definition();
		let mut description = definition.condition.standard_description(definition.subcondition);
		if let Some(message) = self.message().filter(|message| !message.is_empty()) {
			description.push_str(". ");
			description.push_str(&message);
		}
		description
	}

	/// Determines severity while respecting a permitted override.
	pub const fn effective_severity(&self) -> Option<Severity> {
		match self.diagnostic.severity_override() {
			Some(severity) => Some(severity),
			None => self.definition().kind.severity(),
		}
	}

	/// Sets the primary location and returns the updated status or error.
	pub fn with_location(mut self, location: Location) -> Self {
		self.diagnostic = self.diagnostic.with_primary_location(location);
		self
	}

	/// Adds an explanatory location and returns the updated value.
	pub fn with_related_location(mut self, location: LabeledLocation) -> Self {
		self.diagnostic = self.diagnostic.with_related_location(location);
		self
	}

	/// Sets the execution phase and returns the updated value.
	pub fn with_phase(mut self, phase: ExecutionPhase) -> Self {
		self.diagnostic = self.diagnostic.with_phase(phase);
		self
	}

	/// Sets the timestamp and returns the updated value.
	pub fn with_timestamp(mut self, timestamp: SystemTime) -> Self {
		self.diagnostic = self.diagnostic.with_timestamp(timestamp);
		self
	}

	/// Adds a structured extension and returns the updated
	/// value.
	pub fn with_extension<S: Into<String>>(mut self, key: S, value: DiagnosticValue) -> Self {
		self.diagnostic = self.diagnostic.with_extension(key, value);
		self
	}

	/// Validates and sets an allowed status severity.
	pub fn with_severity(mut self, severity: Severity) -> Result<Self, InvalidSeverity> {
		if !self.definition().kind.allows_severity(severity) {
			return Err(InvalidSeverity {
				status: self.code(),
				severity,
			});
		}
		self.diagnostic.set_severity_override(severity);
		Ok(self)
	}

	/// Extracts the typed status owned by this object.
	pub fn into_status(self) -> Status {
		self.status
	}
}

impl From<Status> for StatusObject {
	/// Converts the source value to the target type.
	fn from(status: Status) -> Self {
		Self::new(status)
	}
}

impl Display for StatusObject {
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{}: {}", self.code(), self.description())
	}
}

/// An invalid severity override for the selected status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InvalidSeverity {
	/// The status whose additional context or cause is stored.
	pub status: StatusCode,
	/// The severity specified by the status kind or an invalid override.
	pub severity: Severity,
}

impl Display for InvalidSeverity {
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "severity {:?} is incompatible with status {}", self.severity, self.status)
	}
}

impl Error for InvalidSeverity {}
