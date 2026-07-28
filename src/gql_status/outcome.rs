use std::{
	cmp::Reverse,
	error::Error,
	fmt::{self, Display, Formatter},
};

use crate::gql_status::status::{Condition, Status, StatusObject};

#[derive(Debug, Clone, PartialEq)]
pub struct StatusReport {
	statuses: Vec<StatusObject>,
}

impl StatusReport {
	pub fn new(primary: impl Into<StatusObject>) -> Result<Self, StatusReportError> {
		let primary = primary.into();
		Self::ensure_non_error(&primary)?;
		Ok(Self {
			statuses: vec![primary],
		})
	}

	pub fn successful() -> Self {
		Self {
			statuses: vec![StatusObject::new(Status::successful_completion())],
		}
	}

	pub fn push(&mut self, status: impl Into<StatusObject>) -> Result<(), StatusReportError> {
		let status = status.into();
		Self::ensure_non_error(&status)?;
		self.statuses.push(status);
		self.statuses.sort_by_key(|status| Reverse(precedence(status)));
		Ok(())
	}

	pub fn primary(&self) -> &StatusObject {
		self.statuses.first().expect("StatusReport always contains a primary status")
	}

	pub fn additional(&self) -> &[StatusObject] {
		&self.statuses[1..]
	}

	pub fn iter(&self) -> impl ExactSizeIterator<Item = &StatusObject> {
		self.statuses.iter()
	}

	fn ensure_non_error(status: &StatusObject) -> Result<(), StatusReportError> {
		if status.definition().kind.is_error() {
			Err(StatusReportError {
				status: status.clone(),
			})
		} else {
			Ok(())
		}
	}
}

fn precedence(status: &StatusObject) -> u8 {
	match status.definition().condition {
		Condition::NoData => 4,
		Condition::Warning => 3,
		Condition::SuccessfulCompletion => 2,
		Condition::Informational => 1,
		_ => 0,
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusReportError {
	status: StatusObject,
}

impl StatusReportError {
	pub const fn status(&self) -> &StatusObject {
		&self.status
	}
}

impl Display for StatusReportError {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"error status {} cannot be included in a successful status report",
			self.status.code()
		)
	}
}

impl Error for StatusReportError {}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome<T> {
	value: T,
	statuses: StatusReport,
}

impl<T> Outcome<T> {
	pub fn success(value: T) -> Self {
		Self {
			value,
			statuses: StatusReport::successful(),
		}
	}

	pub const fn new(value: T, statuses: StatusReport) -> Self {
		Self {
			value,
			statuses,
		}
	}

	pub fn with_status(
		mut self,
		status: impl Into<StatusObject>,
	) -> Result<Self, StatusReportError> {
		self.statuses.push(status)?;
		Ok(self)
	}

	pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Outcome<U> {
		Outcome {
			value: map(self.value),
			statuses: self.statuses,
		}
	}

	pub const fn value(&self) -> &T {
		&self.value
	}

	pub const fn statuses(&self) -> &StatusReport {
		&self.statuses
	}

	pub fn into_parts(self) -> (T, StatusReport) {
		(self.value, self.statuses)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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

	#[test]
	fn rejects_errors() {
		let result = Outcome::success(()).with_status(Status::division_by_zero());
		assert!(result.is_err());
	}
}
