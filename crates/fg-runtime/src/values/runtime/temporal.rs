use std::{
	cmp::Ordering,
	fmt::{self, Display, Formatter},
	hash::{Hash, Hasher},
	str::FromStr,
};

use jiff::{Span, Zoned};

/// Runtime timestamp value retaining its time-zone-aware representation.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct DateTime(
	/// Underlying time-zone-aware timestamp.
	Zoned,
);

impl DateTime {
	/// Constructs `DateTime` from the supplied initial state.
	pub const fn new(value: Zoned) -> Self {
		Self(value)
	}

	/// Borrows the wrapped inner representation.
	pub const fn as_inner(&self) -> &Zoned {
		&self.0
	}

	/// Returns the wrapped inner representation.
	pub fn into_inner(self) -> Zoned {
		self.0
	}
}

impl Default for DateTime {
	/// Constructs the default initial state of `DateTime`.
	fn default() -> Self {
		Self(Zoned::UNIX_EPOCH)
	}
}

impl From<Zoned> for DateTime {
	/// Converts `Zoned` into `DateTime`.
	fn from(value: Zoned) -> Self {
		Self(value)
	}
}

impl From<DateTime> for Zoned {
	/// Converts `DateTime` into `Zoned`.
	fn from(value: DateTime) -> Self {
		value.0
	}
}

impl FromStr for DateTime {
	type Err = jiff::Error;

	/// Constructs from str for `DateTime`.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		value.parse().map(Self)
	}
}

impl Display for DateTime {
	/// Formats `DateTime` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, f)
	}
}

/// Runtime duration with the project comparison and serialization interfaces.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct Duration(
	/// Underlying runtime duration representation.
	Span,
);

impl Duration {
	/// Constructs `Duration` from the supplied initial state.
	pub const fn new(value: Span) -> Self {
		Self(value)
	}

	/// Borrows the wrapped inner representation.
	pub const fn as_inner(&self) -> &Span {
		&self.0
	}

	/// Returns the wrapped inner representation.
	pub const fn into_inner(self) -> Span {
		self.0
	}

	/// Returns a fixed-length nanosecond count when the span has no calendar
	/// year or month component.
	pub(crate) fn fixed_nanoseconds(self) -> Option<i128> {
		if self.0.get_years() != 0 || self.0.get_months() != 0 {
			return None;
		}
		Some(
			i128::from(self.0.get_weeks()) * 7 * 86_400_000_000_000
				+ i128::from(self.0.get_days()) * 86_400_000_000_000
				+ i128::from(self.0.get_hours()) * 3_600_000_000_000
				+ i128::from(self.0.get_minutes()) * 60_000_000_000
				+ i128::from(self.0.get_seconds()) * 1_000_000_000
				+ i128::from(self.0.get_milliseconds()) * 1_000_000
				+ i128::from(self.0.get_microseconds()) * 1_000
				+ i128::from(self.0.get_nanoseconds()),
		)
	}

	/// Returns the ordering key result.
	fn ordering_key(self) -> (i64, i64, i128) {
		let months = i64::from(self.0.get_years()) * 12 + i64::from(self.0.get_months());
		let days = i64::from(self.0.get_weeks()) * 7 + i64::from(self.0.get_days());
		let nanoseconds = i128::from(self.0.get_hours()) * 3_600_000_000_000
			+ i128::from(self.0.get_minutes()) * 60_000_000_000
			+ i128::from(self.0.get_seconds()) * 1_000_000_000
			+ i128::from(self.0.get_milliseconds()) * 1_000_000
			+ i128::from(self.0.get_microseconds()) * 1_000
			+ i128::from(self.0.get_nanoseconds());
		(months, days, nanoseconds)
	}
}

impl Default for Duration {
	/// Constructs the default initial state of `Duration`.
	fn default() -> Self {
		Self(Span::new())
	}
}

impl From<Span> for Duration {
	/// Converts `Span` into `Duration`.
	fn from(value: Span) -> Self {
		Self(value)
	}
}

impl From<Duration> for Span {
	/// Converts `Duration` into `Span`.
	fn from(value: Duration) -> Self {
		value.0
	}
}

impl FromStr for Duration {
	type Err = jiff::Error;

	/// Constructs from str for `Duration`.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		value.parse().map(Self)
	}
}

impl Display for Duration {
	/// Formats `Duration` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, f)
	}
}

impl PartialEq for Duration {
	/// Compares this `Duration` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.ordering_key() == other.ordering_key()
	}
}

impl Eq for Duration {}

impl PartialOrd for Duration {
	/// Compares this `Duration` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Duration {
	/// Compares this `Duration` with another value using its ordering contract.
	fn cmp(&self, other: &Self) -> Ordering {
		self.ordering_key().cmp(&other.ordering_key())
	}
}

impl Hash for Duration {
	/// Feeds this `Duration` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.ordering_key().hash(state);
	}
}

/// Operations shared with the engine and value codecs.
pub trait DurationInternal: Sized {
	/// Returns a fixed-length nanosecond count when the span has no calendar
	/// year or month component.
	fn fixed_nanoseconds(self) -> Option<i128>;
}
impl DurationInternal for Duration {
	fn fixed_nanoseconds(self) -> Option<i128> {
		<Duration>::fixed_nanoseconds(self)
	}
}

#[cfg(test)]
/// Regression tests for temporal behavior.
mod tests {
	use super::{DateTime, Duration};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies datetime equality uses instant.
	#[test]
	fn datetime_equality_uses_instant() {
		let utc = "2025-01-01T00:00:00+00:00[UTC]".parse::<DateTime>().assert_ok();
		let new_york =
			"2024-12-31T19:00:00-05:00[America/New_York]".parse::<DateTime>().assert_ok();
		assert_eq!(utc, new_york);
	}

	/// Verifies duration normalizes equivalent units.
	#[test]
	fn duration_normalizes_equivalent_units() {
		let year = "P1Y".parse::<Duration>().assert_ok();
		let months = "P12M".parse::<Duration>().assert_ok();
		let week = "P1W".parse::<Duration>().assert_ok();
		let days = "P7D".parse::<Duration>().assert_ok();
		assert_eq!(year, months);
		assert_eq!(week, days);
	}
}
