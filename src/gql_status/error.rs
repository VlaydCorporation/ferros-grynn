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

#[derive(Clone)]
pub struct FerrosGrynnError {
	inner: Arc<ErrorState>,
}

#[derive(Clone)]
struct ErrorState {
	status: StatusObject,
	cause: Option<FerrosGrynnError>,
	technical_source: Option<Arc<dyn Error + Send + Sync>>,
}

impl FerrosGrynnError {
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

	pub fn status(&self) -> &StatusObject {
		&self.inner.status
	}

	pub fn cause(&self) -> Option<&FerrosGrynnError> {
		self.inner.cause.as_ref()
	}

	pub fn with_cause(mut self, cause: FerrosGrynnError) -> Self {
		Arc::make_mut(&mut self.inner).cause = Some(cause);
		self
	}

	pub fn with_location(mut self, location: Location) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_location(location);
		self
	}

	pub fn with_related_location(mut self, location: LabeledLocation) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_related_location(location);
		self
	}

	pub fn with_phase(mut self, phase: ExecutionPhase) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_phase(phase);
		self
	}

	pub fn with_timestamp(mut self, timestamp: SystemTime) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_timestamp(timestamp);
		self
	}

	pub fn with_extension(mut self, key: impl Into<String>, value: DiagnosticValue) -> Self {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_extension(key, value);
		self
	}

	pub fn with_severity(mut self, severity: Severity) -> Result<Self, InvalidSeverity> {
		let state = Arc::make_mut(&mut self.inner);
		state.status = state.status.clone().with_severity(severity)?;
		Ok(self)
	}

	pub fn mark_critical(self) -> Self {
		self.with_severity(Severity::Critical)
			.expect("critical severity is valid for every error status")
	}

	pub(crate) fn with_technical_source<E>(mut self, source: E) -> Self
	where
		E: Error + Send + Sync + 'static,
	{
		Arc::make_mut(&mut self.inner).technical_source = Some(Arc::new(source));
		self
	}

	pub(crate) fn technical_source(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
		self.inner.technical_source.as_deref()
	}

	/// Returns the root structured FG status.
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

	pub fn accumulator_unknown_field(identifier: &str, field: &str) -> Self {
		Self::struct_build_error(identifier).with_cause(Self::unknown_field(field))
	}

	pub fn infinite_fp() -> Self {
		Self::numeric_value_out_of_range("infinity")
	}

	pub fn out_of_range(
		component: impl Into<String>,
		value_type: impl Into<String>,
		lower: i64,
		upper: i64,
		value: impl Into<String>,
	) -> Self {
		Self::specified_numeric_value_out_of_range(component, value_type, lower, upper, value)
	}
}

impl TryFrom<StatusObject> for FerrosGrynnError {
	type Error = NonErrorStatus;

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
	fn from(source: jiff::Error) -> Self {
		Self::temporal_processing_error().with_technical_source(source)
	}
}

impl Display for FerrosGrynnError {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.inner.status, f)
	}
}

impl Debug for FerrosGrynnError {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.debug_struct("FerrosGrynnError")
			.field("status", &self.inner.status)
			.field("cause", &self.inner.cause)
			.field("technical_source", &self.inner.technical_source.as_ref().map(|_| "<redacted>"))
			.finish()
	}
}

impl Error for FerrosGrynnError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		self.inner.cause.as_ref().map(|cause| cause as &(dyn Error + 'static))
	}
}

#[derive(Debug, Clone)]
pub struct NonErrorStatus(StatusObject);

impl NonErrorStatus {
	pub fn status(&self) -> &StatusObject {
		&self.0
	}
}

impl Display for NonErrorStatus {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{} is not an error status", self.0.code())
	}
}

impl Error for NonErrorStatus {}

#[cfg(test)]
mod tests {
	use super::*;

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
