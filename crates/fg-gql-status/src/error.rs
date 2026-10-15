use std::{
	error::Error,
	fmt::{self, Debug, Display, Formatter},
	sync::Arc,
	time::SystemTime,
};

use crate::gql_status::{
	diagnostic::{ExecutionPhase, LabeledLocation, Location, Severity},
	formatting::DiagnosticValue,
	status::{InvalidSeverity, Status, StatusObject},
};

pub type FerrosGrynnResult<T> = Result<T, FerrosGrynnError>;

/// A project error with a typed status and a structured chain of
/// causes.
#[derive(Clone)]
pub struct FerrosGrynnError {
	/// Shared error contents with their status and causes.
	inner: Arc<ErrorState>,
}

/// Shared error contents separating the public cause from the technical cause.
#[derive(Clone)]
struct ErrorState {
	/// The status whose additional context or cause is stored.
	status: StatusObject,
	/// The immediate structured cause of the error.
	cause: Option<FerrosGrynnError>,
	/// An external technical cause hidden from the public error chain.
	technical_source: Option<Arc<dyn Error + Send + Sync>>,
}

impl FerrosGrynnError {
	/// Creates an error only from an error-completion status.
	pub(crate) fn from_error_status(status: Status) -> Self {
		debug_assert!(
			status.definition().kind.is_error(),
			"only an error status may create FerrosGrynnError"
		);
		Self {
			inner: Arc::new(ErrorState {
				status: StatusObject::new(status),
				cause: None,
				technical_source: None,
			}),
		}
	}

	/// Returns the structured status of this result.
	pub fn status(&self) -> &StatusObject {
		&self.inner.status
	}

	/// Returns the immediate structured cause of the error.
	pub fn cause(&self) -> Option<&FerrosGrynnError> {
		self.inner.cause.as_ref()
	}

	/// Attaches a structured cause and returns the updated error.
	pub fn with_cause(mut self, cause: FerrosGrynnError) -> Self {
		Arc::make_mut(&mut self.inner).cause = Some(cause);
		self
	}

	/// Sets the primary location and returns the updated status or error.
	pub fn with_location(mut self, location: Location) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_location(location);
		self
	}

	/// Adds an explanatory location and returns the updated value.
	pub fn with_related_location(mut self, location: LabeledLocation) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_related_location(location);
		self
	}

	/// Sets the execution phase and returns the updated value.
	pub fn with_phase(mut self, phase: ExecutionPhase) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_phase(phase);
		self
	}

	/// Sets the timestamp and returns the updated value.
	pub fn with_timestamp(mut self, timestamp: SystemTime) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_timestamp(timestamp);
		self
	}

	/// Adds a structured extension and returns the updated
	/// value.
	pub fn with_extension<Str: Into<String>>(mut self, key: Str, value: DiagnosticValue) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_extension(key, value);
		self
	}

	/// Validates and sets an allowed status severity.
	pub fn with_severity(mut self, severity: Severity) -> Result<Self, InvalidSeverity> {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_severity(severity)?;
		Ok(self)
	}

	/// Raises the error severity to critical.
	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "critical severity is valid for every error status"
	)]
	pub fn mark_critical(self) -> Self {
		self.with_severity(Severity::Critical)
			.expect("critical severity is valid for every error status")
	}

	/// Stores an external technical cause separately from the public chain.
	pub fn with_technical_source<E>(mut self, source: E) -> Self
	where
		E: Error + Send + Sync + 'static,
	{
		Arc::make_mut(&mut self.inner).technical_source = Some(Arc::new(source));
		self
	}

	/// Returns the stored technical cause for internal diagnostics.
	pub fn technical_source(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
		self.inner.technical_source.as_deref()
	}

	/// Returns the root structured FG status.
	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "an error chain always contains itself"
	)]
	pub fn root(&self) -> &FerrosGrynnError {
		self.chain().last().expect("an error chain always contains itself")
	}

	/// Iterates structured causes from external context to the root.
	pub fn chain(&self) -> impl Iterator<Item = &FerrosGrynnError> {
		let mut current = self;
		std::iter::once(current).chain(std::iter::from_fn(move || {
			current = current.cause()?;
			Some(current)
		}))
	}

	/// Creates a structured error for an unknown accumulator field.
	pub fn accumulator_unknown_field(identifier: &str, field: &str) -> Self {
		Self::struct_build_error(identifier).with_cause(Self::unknown_field(field))
	}

	/// Creates an error for a disallowed infinite numeric value.
	pub fn infinite_fp() -> Self {
		Self::numeric_value_out_of_range("infinity")
	}

	/// Creates an error for a value outside the allowed range.
	pub fn out_of_range<Str1, Str2, Str3>(
		component: Str1,
		value_type: Str1,
		lower: i64,
		upper: i64,
		value: Str1,
	) -> Self
	where
		Str1: Into<String>,
		Str2: Into<String>,
		Str3: Into<String>,
	{
		Self::specified_numeric_value_out_of_range(component, value_type, lower, upper, value)
	}
}

impl TryFrom<StatusObject> for FerrosGrynnError {
	type Error = NonErrorStatus;

	/// Validates and converts the source value to the target type.
	fn try_from(status: StatusObject) -> Result<Self, Self::Error> {
		if !status.definition().kind.is_error() {
			return Err(NonErrorStatus(status));
		}
		Ok(Self {
			inner: Arc::new(ErrorState {
				status,
				cause: None,
				technical_source: None,
			}),
		})
	}
}

impl From<jiff::Error> for FerrosGrynnError {
	/// Converts the source value to the target type.
	fn from(source: jiff::Error) -> Self {
		Self::temporal_processing_error().with_technical_source(source)
	}
}

impl Display for FerrosGrynnError {
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.inner.status, f)
	}
}

impl Debug for FerrosGrynnError {
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.debug_struct("FerrosGrynnError")
			.field("status", &self.inner.status)
			.field("cause", &self.inner.cause)
			.field("technical_source", &self.inner.technical_source.as_ref().map(|_| "<redacted>"))
			.finish()
	}
}

impl Error for FerrosGrynnError {
	/// Hides the technical cause from the public std::error::Error chain.
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		self.inner.cause.as_ref().map(|cause| cause as &(dyn Error + 'static))
	}
}

/// An error from attempting to use a non-error status as an error completion.
#[derive(Debug, Clone)]
pub struct NonErrorStatus(
	/// The non-error status rejected during error construction.
	StatusObject,
);

impl NonErrorStatus {
	/// Returns the structured status of this result.
	pub const fn status(&self) -> &StatusObject {
		&self.0
	}
}

impl Display for NonErrorStatus {
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{} is not an error status", self.0.code())
	}
}

impl Error for NonErrorStatus {}

#[cfg(test)]
/// Regression tests for error behavior.
mod tests {
	use super::*;

	/// Tests the `structured_chain_is_ordered_from_context_to_root` scenario
	/// and its diagnostic outcome.
	#[test]
	fn structured_chain_is_ordered_from_context_to_root() {
		let error = FerrosGrynnError::struct_build_error("Example")
			.with_cause(FerrosGrynnError::unknown_field("missing"));
		let codes =
			error.chain().map(|error| error.status().code().to_string()).collect::<Vec<_>>();

		assert_eq!(codes, ["FG-22F01", "FG-22F02"]);
		assert_eq!(error.root().status().code().to_string(), "FG-22F02");
		assert!(Error::source(&error).is_some());
	}

	/// Tests the
	/// `technical_source_is_redacted_and_not_in_structured_chain` scenario and
	/// its diagnostic outcome.
	#[test]
	fn technical_source_is_redacted_and_not_in_structured_chain() {
		let external = "not a date".parse::<jiff::civil::Date>().unwrap_err();
		let error = FerrosGrynnError::from(external);

		assert_eq!(error.chain().count(), 1);
		assert!(Error::source(&error).is_none());
		assert!(error.technical_source().is_some());
		assert!(!format!("{error:?}").contains("not a date"));
	}
}
