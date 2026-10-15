//! Fixed-duration capture policy shared by the SDK and query adapters.

use jiff::Span;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Duration,
};

/// Validated retention and capture policy for one class or graph source.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use ferros_grynn::{changefeed::ChangefeedConfig, values::Duration};
/// let config = ChangefeedConfig::new("P7D".parse::<Duration>()?)?.with_full(true);
/// assert!(config.full());
/// assert!(!config.include_original());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChangefeedConfig {
	/// Canonical positive fixed retention duration.
	retention: Duration,
	/// Whether full before images are captured.
	include_original: bool,
	/// Whether successful logical operations are captured.
	full: bool,
}

impl ChangefeedConfig {
	/// Validates and normalizes a positive fixed-length retention span.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error for calendar, nonpositive, or
	/// unrepresentable spans.
	pub fn new(retention: Duration) -> FerrosGrynnResult<Self> {
		let nanos = retention.fixed_nanoseconds().ok_or_else(|| {
			FerrosGrynnError::invalid_argument(
				"changefeed retention cannot contain years or months",
			)
		})?;
		Self::from_nanoseconds(nanos)
	}

	/// Returns the canonical fixed retention span.
	pub const fn retention(self) -> Duration {
		self.retention
	}

	/// Returns whether before images are stored in full.
	pub const fn include_original(self) -> bool {
		self.include_original
	}

	/// Returns whether successful operation history is captured.
	pub const fn full(self) -> bool {
		self.full
	}

	/// Selects full before images instead of reversible before differences.
	#[must_use]
	pub const fn with_include_original(mut self, enabled: bool) -> Self {
		self.include_original = enabled;
		self
	}

	/// Selects operation history, including successful targeted no-op calls.
	#[must_use]
	pub const fn with_full(mut self, enabled: bool) -> Self {
		self.full = enabled;
		self
	}

	/// Constructs a canonical span from its checked durable representation.
	pub(crate) fn from_nanoseconds(nanos: i128) -> FerrosGrynnResult<Self> {
		/// Exact fixed-day normalization unit.
		const NANOS_PER_DAY: i128 = 86_400_000_000_000;
		if nanos <= 0 {
			return Err(FerrosGrynnError::invalid_argument(
				"changefeed retention must be positive",
			));
		}
		let days = i64::try_from(nanos / NANOS_PER_DAY).map_err(|_error| {
			FerrosGrynnError::invalid_argument("changefeed retention exceeds the duration range")
		})?;
		let remainder = i64::try_from(nanos % NANOS_PER_DAY).map_err(|_error| {
			FerrosGrynnError::invalid_argument("changefeed retention remainder is unrepresentable")
		})?;
		let span = Span::new()
			.try_days(days)
			.and_then(|span| span.try_nanoseconds(remainder))
			.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?;
		Ok(Self {
			retention: Duration::new(span),
			include_original: false,
			full: false,
		})
	}
}

#[cfg(test)]
/// Regression tests for config behavior.
mod tests {
	use super::*;

	/// Parses fixture spans through the public runtime duration contract.
	fn config(text: &str) -> FerrosGrynnResult<ChangefeedConfig> {
		ChangefeedConfig::new(
			text.parse().map_err(|error: jiff::Error| {
				FerrosGrynnError::invalid_argument(error.to_string())
			})?,
		)
	}

	/// Verifies retention is positive fixed and canonical.
	#[test]
	fn retention_is_positive_fixed_and_canonical() {
		use crate::test_assertions::AssertResult as _;

		assert_eq!(config("P1D").assert_ok(), config("PT24H").assert_ok());
		assert_eq!(config("P7D").assert_ok(), config("P1W").assert_ok());
		for invalid in ["P1M", "P1Y", "PT0S", "-P1D"] {
			assert!(config(invalid).is_err(), "{invalid}");
		}
		let policy = config("P1D").assert_ok().with_full(true).with_include_original(true);
		assert!(policy.full());
		assert!(policy.include_original());
		assert_ne!(policy, config("P1D").assert_ok());
	}
}
