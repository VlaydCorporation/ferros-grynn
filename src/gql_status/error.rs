use std::fmt::{Display, Formatter};
use crate::gql_status::diagnostic::{DiagnosticRecord, Severity};
use crate::gql_status::status::{ExecutionPhase, Status, StatusMeta, StatusObject};
use jiff::Error;
use std::sync::Arc;
use thiserror::Error;

pub type FerrosGrynnResult<T> = core::result::Result<T, FerrosGrynnError>;

#[derive(Error, Debug, Clone)]
#[error("{inner}")]
pub struct FerrosGrynnError {
	inner: Arc<ErrorState>,
}

impl FerrosGrynnError {
	pub fn conversation_error(value: String, ty: impl Into<String>) -> Self {
		Status::ConversationError {
			value,
			valuetype: ty.into()
		}.into()
	}

	pub fn atom_does_not_exist(id: String, ty: String) -> Self {
		Status::AtomDoesNotExists {
			atomtype: ty,
			id
		}.into()
	}

	pub fn empty_edge_targets() -> Self {
		Status::EmptyEdgeTargets.into()
	}

	pub fn edge_creation_error() -> Self {
		Status::EdgeCreationError.into()
	}

	pub fn invalid_argument(message: String) -> FerrosGrynnError {
		Status::InvalidArgument {
			message,
		}.into()
	}

	pub fn operation_overflow(operation: &str) -> FerrosGrynnError {
		FerrosGrynnErrorBuilder::from_status(Status::IntervalFieldOverflow)
			.with_cause(Status::OverflowError {
				operation: operation.to_string(),
			})
			.build()
	}

	pub fn division_by_zero() -> FerrosGrynnError {
		Status::DivizionByZero.into()
	}

	pub fn infinite_fp() -> FerrosGrynnError {
		Status::InfiniteFloatingPointValue.into()
	}

	pub fn out_of_range(component: String, valuetype: String, lower: i64, upper: i64, value: String) -> FerrosGrynnError {
		Status::SpecifiedNumericValueOutOfRange {
			component,
			valuetype,
			lower,
			upper,
			value,
		}.into()
	}

	pub fn accumulator_unknown_field(ident: &str, field: &str) -> FerrosGrynnError {
		FerrosGrynnErrorBuilder::from_status(Status::StructBuildError {
			ident: ident.to_string(),
		})
		.with_cause(Status::UnknownField {
			field: field.to_string(),
		})
		.build()
	}
}

pub struct FerrosGrynnErrorBuilder {
	state: ErrorState,
}

impl FerrosGrynnErrorBuilder {
	pub fn from_status(status: Status) -> Self {
		Self {
			state: ErrorState {
				status: DiagnosticRecord::error_record(StatusObject {
					status,
					meta: StatusMeta::default(),
				}),
				cause: None,
			},
		}
	}

	pub fn with_cause(mut self, cause: impl Into<FerrosGrynnError>) -> Self {
		self.state.cause = Some(Arc::new(cause.into()));
		self
	}

	pub fn mark_critical(mut self) -> Self {
		self.state.status.severity = Severity::Critical;
		self
	}

	pub fn at_location(mut self, location: Location) -> Self {
		self.state.status.status.meta.location = Some(location);
		self
	}

	pub fn at_time(mut self, time: std::time::SystemTime) -> Self {
		self.state.status.status.meta.timestamp = Some(time);
		self
	}

	pub fn at_phase(mut self, phase: ExecutionPhase) -> Self {
		self.state.status.status.meta.execution_phase = Some(phase);
		self
	}

	pub fn build(self) -> FerrosGrynnError {
		FerrosGrynnError {
			inner: Arc::new(self.state),
		}
	}
}

#[derive(Error, Debug)]
struct ErrorState {
	status: DiagnosticRecord,
	#[source]
	cause: Option<Arc<FerrosGrynnError>>,
}

impl Display for ErrorState {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		todo!()
	}
}

impl FerrosGrynnError {
	/// Returns the root error in this chain.
	pub fn root(&self) -> &FerrosGrynnError {
		// OK because `Error::chain` is guaranteed to return a non-empty
		// iterator.
		self.chain().last().unwrap()
	}

	/// Returns a chain of error values.
	///
	/// This starts with the most recent error added to the chain. That is,
	/// the highest level context. The last error in the chain is always the
	/// "root" cause. That is, the error closest to the point where something
	/// has gone wrong.
	///
	/// The iterator returned is guaranteed to yield at least one error.
	pub fn chain(&self) -> impl Iterator<Item = &FerrosGrynnError> {
		let mut err = self;
		core::iter::once(err).chain(core::iter::from_fn(move || {
			err = err.inner.as_ref().cause.as_ref()?;
			Some(err)
		}))
	}
}

impl From<StatusObject> for FerrosGrynnError {
	fn from(status: StatusObject) -> Self {
		FerrosGrynnError {
			inner: Arc::new(ErrorState {
				status: DiagnosticRecord::error_record(status),
				cause: None,
			}),
		}
	}
}

impl From<Status> for FerrosGrynnError {
	fn from(status: Status) -> Self {
		FerrosGrynnError {
			inner: Arc::new(ErrorState {
				status: DiagnosticRecord::error_record(StatusObject {
					status,
					meta: StatusMeta::default(),
				}),
				cause: None,
			}),
		}
	}
}

impl From<jiff::Error> for FerrosGrynnError {
	fn from(value: Error) -> Self {
		FerrosGrynnErrorBuilder::from_status(Status::UnknownExternalError {
			message: value.to_string(),
		})
		.build()
	}
}
