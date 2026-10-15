use serde::{Deserialize, Serialize};

use super::StatusCode;
use crate::diagnostic::Severity;

/// The standard completion condition that determines the GQL status-code class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum Condition {
	/// Successful completion without the requested data.
	NoData,
	/// A warning accompanying operation completion.
	Warning,
	/// Successful completion of the operation.
	SuccessfulCompletion,
	/// An informational completion condition.
	Informational,
	/// Invalid or unrepresentable data.
	DataException,
	/// An error related to query execution.
	QueryException,
	/// A syntax or access-rule violation.
	SyntaxErrorOrAccessRuleViolation,
	/// A general processing error.
	GeneralProcessingException,
	/// An error in system configuration or a system operation.
	SystemConfigurationOrOperationException,
	/// An error in a called procedure or function.
	ProcedureException,
	/// A dependent-object constraint violation.
	DependentObjectError,
	/// A violation of a graph type's structural constraints.
	GraphTypeViolation,
	/// An operation disallowed in the current transaction state.
	InvalidTransactionState,
	/// An invalid transaction termination.
	InvalidTransactionTermination,
	/// The transaction must be rolled back.
	TransactionRollback,
	/// A configured software limit has been exceeded.
	ProgramLimitExceeded,
}

impl Condition {
	/// Returns a human-readable description of the status or condition.
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

	/// Returns the standard description of the condition class.
	pub fn standard_description(self, subcondition: Option<&str>) -> String {
		match subcondition {
			Some(subcondition) if !subcondition.is_empty() => {
				format!("{} - {subcondition}", self.description())
			}
			_ => self.description().to_owned(),
		}
	}
}

/// A notification category for client filtering and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum NotificationClassification {
	/// A deprecated feature was used.
	Deprecation,
	/// Guidance on using the operation.
	Hint,
	/// A condition that affects performance.
	Performance,
	/// A general-purpose notification.
	Generic,
	/// An unrecognized input or schema element.
	Unrecognised,
	/// An unsupported feature was requested.
	Unsupported,
	/// A security-related notification.
	Security,
	/// A notification about graph structure.
	Topology,
	/// A notification about the data schema.
	Schema,
}

/// The error classification that determines the expected client response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum ErrorClassification {
	/// The client must correct the query or input data.
	ClientError,
	/// The error originates on the database side.
	DatabaseError,
	/// A transient condition under which a retry may
	/// succeed.
	TransientError,
}

/// The semantic error category, separate from the detection phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum ErrorCategory {
	/// An error in the query's syntactic form.
	Syntax,
	/// An error in the query's semantic constraints.
	Semantic,
	/// Incompatible value or operation types.
	Type,
	/// A transaction-state or coordination error.
	Transaction,
	/// An inability to construct a valid plan.
	Planning,
	/// An error detected during execution.
	Runtime,
}

/// The subsystem responsible for the reported status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum Domain {
	/// Query processing and execution.
	Query,
	/// Values, types, and conversions.
	Value,
	/// Graph structure and operations.
	Graph,
	/// Durable storage and physical formats.
	Storage,
	/// Transactions and concurrency control.
	Transaction,
	/// System input/output operations.
	Io,
	/// Configuration settings.
	Configuration,
	/// Access rights and security.
	Security,
	/// An external system or library.
	External,
}

/// An allowed combination of status kind, classification, and severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum StatusKind {
	/// Operation completion without a notification or error.
	Completion,
	/// A notification with its own classification and severity.
	Notification {
		/// The notification or error classification for client handling.
		classification: NotificationClassification,
		/// The severity specified by the status kind or an invalid
		/// override.
		severity: Severity,
	},
	/// An error with a classification, category, and severity.
	Error {
		/// The notification or error classification for client handling.
		classification: ErrorClassification,
		/// The semantic error category, if applicable.
		category: ErrorCategory,
		/// The severity specified by the status kind or an invalid
		/// override.
		severity: Severity,
	},
}

impl StatusKind {
	/// Returns the severity specified by the status kind.
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

	/// Returns the error classification if the status describes an error.
	pub const fn error_classification(self) -> Option<ErrorClassification> {
		match self {
			Self::Error {
				classification,
				..
			} => Some(classification),
			_ => None,
		}
	}

	/// Returns the notification classification if the status describes a
	/// notification.
	pub const fn notification_classification(self) -> Option<NotificationClassification> {
		match self {
			Self::Notification {
				classification,
				..
			} => Some(classification),
			_ => None,
		}
	}

	/// Returns the error category if it applies to this status.
	pub const fn error_category(self) -> Option<ErrorCategory> {
		match self {
			Self::Error {
				category,
				..
			} => Some(category),
			_ => None,
		}
	}

	/// Checks whether the status represents an error completion.
	pub const fn is_error(self) -> bool {
		matches!(self, Self::Error { .. })
	}

	/// Checks whether the specified severity is allowed for this status kind.
	pub const fn allows_severity(self, severity: Severity) -> bool {
		matches!(
			(self, severity),
			(Self::Notification { .. }, Severity::Information | Severity::Warning)
				| (Self::Error { .. }, Severity::Error | Severity::Critical)
		)
	}
}

/// A static description of the code, condition, subsystem, and permitted
/// semantics of the status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
	clippy::partial_pub_fields,
	reason = "generated status metadata is public while the template implementation flag is \
	          crate-internal"
)]
pub struct StatusDefinition {
	/// The symbolic definition name in the status catalog.
	pub name: &'static str,
	/// The stable code that determines the status kind.
	pub code: StatusCode,
	/// The standard completion condition that determines the code class.
	pub condition: Condition,
	/// An optional refinement of the standard completion condition.
	pub subcondition: Option<&'static str>,
	/// The subsystem responsible for the status.
	pub domain: Domain,
	/// The status kind with its permitted classification and severity.
	pub kind: StatusKind,
	/// An optional hint for handling the condition.
	pub hint: Option<&'static str>,
	/// Whether the status definition contains a formattable message.
	pub(crate) has_message: bool,
}
