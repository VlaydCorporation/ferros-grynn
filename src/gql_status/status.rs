use std::{
	collections::BTreeMap,
	error::Error,
	fmt::{self, Display, Formatter},
	str::FromStr,
	time::SystemTime,
};

use ferros_grynn_macros::define_status_codes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::gql_status::{
	diagnostic::{DiagnosticRecord, ExecutionPhase, LabeledLocation, Location, Severity},
	formatting::{DiagnosticValue, DisplayValue, Identifier, StringLiteral, ValueType},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StatusCode([u8; 5]);

impl StatusCode {
	pub const fn new(body: [u8; 5]) -> Self {
		let mut index = 0;
		while index < body.len() {
			let byte = body[index];
			assert!(
				(byte >= b'0' && byte <= b'9') || (byte >= b'A' && byte <= b'Z'),
				"status code contains an invalid byte"
			);
			index += 1;
		}
		Self(body)
	}

	pub fn body(&self) -> &str {
		// SAFETY: constructors validate that every byte is ASCII.
		unsafe { std::str::from_utf8_unchecked(&self.0) }
	}
}

impl Display for StatusCode {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "FG-{}", self.body())
	}
}

impl FromStr for StatusCode {
	type Err = StatusCodeParseError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let body = value.strip_prefix("FG-").ok_or(StatusCodeParseError::MissingPrefix)?;
		if body.len() != 5 {
			return Err(StatusCodeParseError::InvalidLength);
		}
		let mut bytes = [0_u8; 5];
		bytes.copy_from_slice(body.as_bytes());
		if !bytes.iter().all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase()) {
			return Err(StatusCodeParseError::InvalidCharacter);
		}
		Ok(Self(bytes))
	}
}

impl Serialize for StatusCode {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.collect_str(self)
	}
}

impl<'de> Deserialize<'de> for StatusCode {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = String::deserialize(deserializer)?;
		value.parse().map_err(serde::de::Error::custom)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCodeParseError {
	MissingPrefix,
	InvalidLength,
	InvalidCharacter,
}

impl Display for StatusCodeParseError {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::MissingPrefix => f.write_str("status code must start with FG-"),
			Self::InvalidLength => f.write_str("status code body must contain five characters"),
			Self::InvalidCharacter => {
				f.write_str("status code must contain only ASCII uppercase letters and digits")
			}
		}
	}
}

impl Error for StatusCodeParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Condition {
	NoData,
	Warning,
	SuccessfulCompletion,
	Informational,
	DataException,
	QueryException,
	SyntaxErrorOrAccessRuleViolation,
	GeneralProcessingException,
	SystemConfigurationOrOperationException,
	ProcedureException,
	DependentObjectError,
	GraphTypeViolation,
	InvalidTransactionState,
	InvalidTransactionTermination,
	TransactionRollback,
	ProgramLimitExceeded,
}

impl Condition {
	pub const fn description(self) -> &'static str {
		match self {
			Self::NoData => "note: no data",
			Self::Warning => "warn",
			Self::SuccessfulCompletion => "note: successful completion",
			Self::Informational => "info",
			Self::DataException => "error: data exception",
			Self::QueryException => "error: query exception",
			Self::SyntaxErrorOrAccessRuleViolation => {
				"error: syntax error or access rule violation"
			}
			Self::GeneralProcessingException => "error: general processing exception",
			Self::SystemConfigurationOrOperationException => {
				"error: system configuration or operation exception"
			}
			Self::ProcedureException => "error: procedure exception",
			Self::DependentObjectError => "error: dependent object error",
			Self::GraphTypeViolation => "error: graph type violation",
			Self::InvalidTransactionState => "error: invalid transaction state",
			Self::InvalidTransactionTermination => "error: invalid transaction termination",
			Self::TransactionRollback => "error: transaction rollback",
			Self::ProgramLimitExceeded => "error: program limit exceeded",
		}
	}

	pub fn standard_description(self, subcondition: Option<&str>) -> String {
		match subcondition {
			Some(subcondition) if !subcondition.is_empty() => {
				format!("{} - {subcondition}", self.description())
			}
			_ => self.description().to_owned(),
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NotificationClassification {
	Deprecation,
	Hint,
	Performance,
	Generic,
	Unrecognised,
	Unsupported,
	Security,
	Topology,
	Schema,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorClassification {
	ClientError,
	DatabaseError,
	TransientError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCategory {
	Syntax,
	Semantic,
	Type,
	Transaction,
	Planning,
	Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Domain {
	Query,
	Graph,
	Storage,
	Transaction,
	Io,
	Configuration,
	Security,
	External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StatusKind {
	Completion,
	Notification {
		classification: NotificationClassification,
		severity: Severity,
	},
	Error {
		classification: ErrorClassification,
		category: ErrorCategory,
		severity: Severity,
	},
}

impl StatusKind {
	pub const fn severity(self) -> Option<Severity> {
		match self {
			Self::Completion => None,
			Self::Notification {
				severity,
				..
			}
			| Self::Error {
				severity,
				..
			} => Some(severity),
		}
	}

	pub const fn error_classification(self) -> Option<ErrorClassification> {
		match self {
			Self::Error {
				classification,
				..
			} => Some(classification),
			_ => None,
		}
	}

	pub const fn notification_classification(self) -> Option<NotificationClassification> {
		match self {
			Self::Notification {
				classification,
				..
			} => Some(classification),
			_ => None,
		}
	}

	pub const fn error_category(self) -> Option<ErrorCategory> {
		match self {
			Self::Error {
				category,
				..
			} => Some(category),
			_ => None,
		}
	}

	pub const fn is_error(self) -> bool {
		matches!(self, Self::Error { .. })
	}

	pub const fn allows_severity(self, severity: Severity) -> bool {
		matches!(
			(self, severity),
			(Self::Notification { .. }, Severity::Information | Severity::Warning)
				| (Self::Error { .. }, Severity::Error | Severity::Critical)
		)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusDefinition {
	pub name: &'static str,
	pub code: StatusCode,
	pub condition: Condition,
	pub subcondition: Option<&'static str>,
	pub domain: Domain,
	pub kind: StatusKind,
	pub hint: Option<&'static str>,
	pub(crate) has_message: bool,
}

define_status_codes!(
	SuccessfulCompletion, "FG-00000" => {
		kind: Completion,
		condition: Condition::SuccessfulCompletion,
		domain: Domain::Query,
	},
	OmittedResult, "FG-00001" => {
		kind: Completion,
		condition: Condition::SuccessfulCompletion,
		domain: Domain::Query,
		subcondition: "omitted result",
	},
	NoData, "FG-02000" => {
		kind: Completion,
		condition: Condition::NoData,
		domain: Domain::Query,
	},
	DeprecatedFeature, "FG-01F01" => {
		kind: Notification {
			classification: NotificationClassification::Deprecation,
			severity: Severity::Warning,
		},
		condition: Condition::Warning,
		domain: Domain::Query,
		subcondition: "deprecated feature",
		message: "{feature} is deprecated; use {replacement} instead.",
		params: {
			feature: String => Identifier,
			replacement: String => Identifier,
		},
	},
	PerformanceAdvisory, "FG-03F01" => {
		kind: Notification {
			classification: NotificationClassification::Performance,
			severity: Severity::Information,
		},
		condition: Condition::Informational,
		domain: Domain::Query,
		subcondition: "performance advisory",
		message: "{message}",
		params: {
			message: String => DisplayValue,
		},
	},
	InvalidArgument, "FG-22N11" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "invalid argument",
		message: "{message}",
		params: {
			message: String => DisplayValue,
		},
	},
	ConversionError, "FG-22N37" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "invalid coercion",
		message: "Cannot convert {value} to {target_type}; the rejected value was {value}.",
		params: {
			value: String => StringLiteral,
			target_type: String => ValueType,
		},
	},
	DivisionByZero, "FG-22012" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "division by zero",
	},
	IntervalFieldOverflow, "FG-22015" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "interval field overflow",
	},
	OperationOverflow, "FG-22N28" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "operation overflow",
		message: "The result of operation {operation} cannot be represented.",
		params: {
			operation: String => StringLiteral,
		},
	},
	NumericValueOutOfRange, "FG-22003" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "numeric value out of range",
		message: "The numeric value {value} is outside the required range.",
		params: {
			value: String => DisplayValue,
		},
	},
	SpecifiedNumericValueOutOfRange, "FG-22N03" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "specified numeric value out of range",
		message: "Expected {component} to be of type {value_type} in the range {lower} to {upper}, but found {value}.",
		params: {
			component: String => StringLiteral,
			value_type: String => ValueType,
			lower: i64 => DisplayValue,
			upper: i64 => DisplayValue,
			value: String => DisplayValue,
		},
	},
	StructBuildError, "FG-22F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "struct build error",
		message: "Cannot build struct {identifier}.",
		params: {
			identifier: String => Identifier,
		},
	},
	UnknownField, "FG-22F02" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Semantic,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "unknown field",
		message: "Unknown field {field}.",
		params: {
			field: String => Identifier,
		},
	},
	NonExactDivision, "FG-22F03" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "non-exact division",
	},
	TemporalProcessingError, "FG-22F04" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::External,
		subcondition: "temporal value processing failed",
		message: "The temporal value could not be processed.",
	},
	AtomDoesNotExist, "FG-G1F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DependentObjectError,
		domain: Domain::Graph,
		subcondition: "atom does not exist",
		message: "The atom of type {atom_type} with id {id} does not exist.",
		params: {
			atom_type: String => ValueType,
			id: String => DisplayValue,
		},
	},
	EmptyEdgeTargets, "FG-G2F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::GraphTypeViolation,
		domain: Domain::Graph,
		subcondition: "edge targets are empty",
		message: "An edge must have at least one target.",
	},
	EdgeCreationError, "FG-G2F02" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::GraphTypeViolation,
		domain: Domain::Graph,
		subcondition: "cannot create edge",
	},
	QueryTooLarge, "FG-54F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Syntax,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Query,
		subcondition: "query too large",
		message: "The query exceeds the maximum supported size of 4,294,967,295 bytes.",
	},
	QueryParseError, "FG-42001" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Syntax,
			severity: Severity::Error,
		},
		condition: Condition::SyntaxErrorOrAccessRuleViolation,
		domain: Domain::Query,
		subcondition: "invalid syntax",
		message: "{message}",
		params: {
			message: String => DisplayValue,
		},
	},
	UnexpectedExternalFailure, "FG-50F00" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::GeneralProcessingException,
		domain: Domain::External,
		subcondition: "unexpected external component failure",
		message: "Component {component} failed while performing {operation}.",
		params: {
			component: String => Identifier,
			operation: String => StringLiteral,
		},
	}
);

impl Display for Status {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		self.fmt_message(f)
	}
}

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

	pub fn with_extension(mut self, key: impl Into<String>, value: DiagnosticValue) -> Self {
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_and_formats_status_code() {
		let code = "FG-22N28".parse::<StatusCode>().unwrap();
		assert_eq!(code.body(), "22N28");
		assert_eq!(code.to_string(), "FG-22N28");
		assert!("22N28".parse::<StatusCode>().is_err());
	}

	#[test]
	fn named_parameter_may_be_used_twice() {
		let status = Status::conversion_error("42", "integer");
		assert_eq!(
			status.message().unwrap(),
			"Cannot convert '42' to integer; the rejected value was '42'."
		);
		assert_eq!(status.parameters().len(), 2);
	}

	#[test]
	fn status_object_builds_full_description() {
		let status = StatusObject::new(Status::division_by_zero());
		assert_eq!(status.to_string(), "FG-22012: error: data exception - division by zero");
	}
}
