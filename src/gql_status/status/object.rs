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

#[derive(Debug, Clone, PartialEq)]
pub struct StatusObject {
	status: Status,
	diagnostic: DiagnosticRecord,
}

impl StatusObject {
	pub fn new(status: Status) -> Self {
		Self {
			status,
			diagnostic: DiagnosticRecord::default(),
		}
	}

	pub const fn status(&self) -> &Status {
		&self.status
	}

	pub const fn definition(&self) -> &'static StatusDefinition {
		self.status.definition()
	}

	pub const fn code(&self) -> StatusCode {
		self.definition().code
	}

	pub const fn condition(&self) -> Condition {
		self.definition().condition
	}

	pub const fn domain(&self) -> Domain {
		self.definition().domain
	}

	pub const fn hint(&self) -> Option<&'static str> {
		self.definition().hint
	}

	pub const fn error_classification(&self) -> Option<ErrorClassification> {
		self.definition().kind.error_classification()
	}

	pub const fn notification_classification(&self) -> Option<NotificationClassification> {
		self.definition().kind.notification_classification()
	}

	pub const fn error_category(&self) -> Option<ErrorCategory> {
		self.definition().kind.error_category()
	}

	pub const fn diagnostic(&self) -> &DiagnosticRecord {
		&self.diagnostic
	}

	pub fn parameters(&self) -> BTreeMap<&'static str, DiagnosticValue> {
		self.status.parameters()
	}

	pub fn message(&self) -> Option<String> {
		self.status.message()
	}

	pub fn description(&self) -> String {
		let definition = self.definition();
		let mut description = definition.condition.standard_description(definition.subcondition);
		if let Some(message) = self.message().filter(|message| !message.is_empty()) {
			description.push_str(". ");
			description.push_str(&message);
		}
		description
	}

	pub const fn effective_severity(&self) -> Option<Severity> {
		match self.diagnostic.severity_override() {
			Some(severity) => Some(severity),
			None => self.definition().kind.severity(),
		}
	}

	pub fn with_location(mut self, location: Location) -> Self {
		self.diagnostic = self.diagnostic.with_primary_location(location);
		self
	}

	pub fn with_related_location(mut self, location: LabeledLocation) -> Self {
		self.diagnostic = self.diagnostic.with_related_location(location);
		self
	}

	pub fn with_phase(mut self, phase: ExecutionPhase) -> Self {
		self.diagnostic = self.diagnostic.with_phase(phase);
		self
	}

	pub fn with_timestamp(mut self, timestamp: SystemTime) -> Self {
		self.diagnostic = self.diagnostic.with_timestamp(timestamp);
		self
	}

	pub fn with_extension<S: Into<String>>(mut self, key: S, value: DiagnosticValue) -> Self {
		self.diagnostic = self.diagnostic.with_extension(key, value);
		self
	}

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

	pub fn into_status(self) -> Status {
		self.status
	}
}

impl From<Status> for StatusObject {
	fn from(status: Status) -> Self {
		Self::new(status)
	}
}

impl Display for StatusObject {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{}: {}", self.code(), self.description())
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InvalidSeverity {
	pub status: StatusCode,
	pub severity: Severity,
}

impl Display for InvalidSeverity {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "severity {:?} is incompatible with status {}", self.severity, self.status)
	}
}

impl Error for InvalidSeverity {}
