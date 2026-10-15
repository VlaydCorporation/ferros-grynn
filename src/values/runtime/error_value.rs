use crate::gql_status::FerrosGrynnError;

/// Runtime-only, storage-rejected projection exposed to a GrynnQL `CATCH`.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct ErrorValue {
	/// Code associated with this error value.
	code: String,
	/// Message associated with this error value.
	message: String,
	/// Condition associated with this error value.
	condition: String,
	/// Classification associated with this error value.
	classification: String,
	/// Category associated with this error value.
	category: String,
	/// Domain associated with this error value.
	domain: String,
	/// Causes associated with this error value.
	causes: Vec<ErrorValue>,
}

impl ErrorValue {
	/// Creates from error.
	pub(crate) fn from_error(error: &FerrosGrynnError) -> Self {
		let status = error.status();
		Self {
			code: status.code().to_string(),
			message: status.message().unwrap_or_else(|| status.description()),
			condition: format!("{:?}", status.condition()),
			classification: status
				.error_classification()
				.map_or_else(String::new, |value| format!("{value:?}")),
			category: status
				.error_category()
				.map_or_else(String::new, |value| format!("{value:?}")),
			domain: format!("{:?}", status.domain()),
			causes: error.cause().map(Self::from_error).into_iter().collect(),
		}
	}

	/// Returns the stable status code of the caught error.
	pub fn code(&self) -> &str {
		&self.code
	}

	/// Returns the rendered diagnostic message.
	pub fn message(&self) -> &str {
		&self.message
	}

	/// Returns the GQLSTATUS condition name.
	pub fn condition(&self) -> &str {
		&self.condition
	}

	/// Returns the diagnostic classification name, or an empty string.
	pub fn classification(&self) -> &str {
		&self.classification
	}

	/// Returns the diagnostic category name, or an empty string.
	pub fn category(&self) -> &str {
		&self.category
	}

	/// Returns the project error-domain name.
	pub fn domain(&self) -> &str {
		&self.domain
	}

	/// Returns the recursively projected cause chain.
	pub fn causes(&self) -> &[Self] {
		&self.causes
	}
}
