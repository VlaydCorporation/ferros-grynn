use serde::{Deserialize, Serialize};

use super::StatusCode;
use crate::diagnostic::Severity;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub enum ErrorClassification {
	ClientError,
	DatabaseError,
	TransientError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
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
#[non_exhaustive]
pub enum Domain {
	Query,
	Value,
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
#[non_exhaustive]
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
#[allow(
	clippy::partial_pub_fields,
	reason = "generated status metadata is public while the template implementation flag is \
	          crate-internal"
)]
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
