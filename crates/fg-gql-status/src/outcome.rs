use std::{
	cmp::Reverse,
	error::Error,
	fmt::{self, Display, Formatter},
};

use crate::status::{Condition, Status, StatusObject};

/// Non-error statuses with a primary status selected by GQL precedence rules.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusReport {
	/// Completion and notification statuses of a successful result.
	statuses: Vec<StatusObject>,
}

impl StatusReport {
	/// Creates a value from the supplied inputs.
	pub fn new<IntoSO: Into<StatusObject>>(primary: IntoSO) -> Result<Self, StatusReportError> {
		let primary = primary.into();
		Self::ensure_non_error(&primary)?;
		Ok(Self {
			statuses: vec![primary],
		})
	}

	/// Creates a successful-completion report without additional notifications.
	pub fn successful() -> Self {
		Self {
			statuses: vec![StatusObject::new(Status::successful_completion())],
		}
	}

	/// Adds a non-error status according to GQL precedence rules.
	pub fn push<IntoSO: Into<StatusObject>>(
		&mut self,
		status: IntoSO,
	) -> Result<(), StatusReportError> {
		let status = status.into();
		Self::ensure_non_error(&status)?;
		self.statuses.push(status);
		self.statuses.sort_by_key(|status| Reverse(precedence(status)));
		Ok(())
	}

	/// Returns the primary status selected by GQL precedence rules.
	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "StatusReport always contains a primary status"
	)]
	pub fn primary(&self) -> &StatusObject {
		self.statuses.first().expect("StatusReport always contains a primary status")
	}

	/// Returns the report's additional non-error statuses.
	pub fn additional(&self) -> &[StatusObject] {
		&self.statuses[1..]
	}

	/// Iterates over report statuses, starting with the primary status.
	pub fn iter(&self) -> impl ExactSizeIterator<Item = &StatusObject> {
		self.statuses.iter()
	}

	/// Rejects an error-completion status when constructing a non-error
	/// report.
	fn ensure_non_error(status: &StatusObject) -> Result<(), StatusReportError> {
		if status.definition().kind.is_error() {
			Err(StatusReportError {
				status: Box::new(status.clone()),
			})
		} else {
			Ok(())
		}
	}
}

/// Returns the condition priority used to select the primary GQL status.
const fn precedence(status: &StatusObject) -> u8 {
	match status.definition().condition {
		Condition::NoData => 4,
		Condition::Warning => 3,
		Condition::SuccessfulCompletion => 2,
		Condition::Informational => 1,
		_ => 0,
	}
}

/// A rejected error status that cannot be included in a non-error report.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusReportError {
	/// The status whose additional context or cause is stored.
	status: Box<StatusObject>,
}

impl StatusReportError {
	/// Returns the structured status of this result.
	pub fn status(&self) -> &StatusObject {
		&self.status
	}
}

impl Display for StatusReportError {
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"error status {} cannot be included in a successful status report",
			self.status.code()
		)
	}
}

impl Error for StatusReportError {}

/// A successful result value together with its completion statuses and
/// notifications.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome<T> {
	/// The value to which formatting or a status report is applied.
	value: T,
	/// Completion and notification statuses of a successful result.
	statuses: StatusReport,
}

impl<T> Outcome<T> {
	/// Creates a successful result with the supplied value.
	pub fn success(value: T) -> Self {
		Self {
			value,
			statuses: StatusReport::successful(),
		}
	}

	/// Creates a value from the supplied inputs.
	pub const fn new(value: T, statuses: StatusReport) -> Self {
		Self {
			value,
			statuses,
		}
	}

	/// Adds a completion or notification status to the result.
	pub fn with_status<IntoSO: Into<StatusObject>>(
		mut self,
		status: IntoSO,
	) -> Result<Self, StatusReportError> {
		self.statuses.push(status)?;
		Ok(self)
	}

	/// Transforms the result value while preserving its status report.
	pub fn map<U, MapFn: FnOnce(T) -> U>(self, map: MapFn) -> Outcome<U> {
		Outcome {
			value: map(self.value),
			statuses: self.statuses,
		}
	}

	/// Returns a reference to the result value.
	pub const fn value(&self) -> &T {
		&self.value
	}

	/// Returns the result's non-error status report.
	pub const fn statuses(&self) -> &StatusReport {
		&self.statuses
	}

	/// Splits the result into its owned value and status report.
	pub fn into_parts(self) -> (T, StatusReport) {
		(self.value, self.statuses)
	}
}

#[cfg(test)]
/// Regression tests for outcome behavior.
mod tests {
	use super::*;

	/// Tests the `applies_gql_precedence` scenario and its diagnostic
	/// outcome.
	#[test]
	fn applies_gql_precedence() {
		let report = Outcome::success(42)
			.with_status(Status::performance_advisory("scan"))
			.unwrap()
			.with_status(Status::deprecated_feature("old", "new"))
			.unwrap()
			.with_status(Status::no_data())
			.unwrap();

		assert_eq!(report.statuses().primary().code().to_string(), "FG-02000");
		assert_eq!(report.statuses().iter().count(), 4);
	}

	/// Tests the `rejects_errors` scenario and its diagnostic outcome.
	#[test]
	fn rejects_errors() {
		let result = Outcome::success(()).with_status(Status::division_by_zero());
		assert!(result.is_err());
	}
}
