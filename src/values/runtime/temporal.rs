use std::{
	cmp::Ordering,
	fmt::{self, Display, Formatter},
	hash::{Hash, Hasher},
	str::FromStr,
};

use jiff::{Span, Zoned};

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct DateTime(Zoned);

impl DateTime {
	pub const fn new(value: Zoned) -> Self {
		Self(value)
	}

	pub const fn as_inner(&self) -> &Zoned {
		&self.0
	}

	pub fn into_inner(self) -> Zoned {
		self.0
	}
}

impl Default for DateTime {
	fn default() -> Self {
		Self(Zoned::UNIX_EPOCH)
	}
}

impl From<Zoned> for DateTime {
	fn from(value: Zoned) -> Self {
		Self(value)
	}
}

impl From<DateTime> for Zoned {
	fn from(value: DateTime) -> Self {
		value.0
	}
}

impl FromStr for DateTime {
	type Err = jiff::Error;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		value.parse().map(Self)
	}
}

impl Display for DateTime {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, f)
	}
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct Duration(Span);

impl Duration {
	pub const fn new(value: Span) -> Self {
		Self(value)
	}

	pub const fn as_inner(&self) -> &Span {
		&self.0
	}

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
	fn default() -> Self {
		Self(Span::new())
	}
}

impl From<Span> for Duration {
	fn from(value: Span) -> Self {
		Self(value)
	}
}

impl From<Duration> for Span {
	fn from(value: Duration) -> Self {
		value.0
	}
}

impl FromStr for Duration {
	type Err = jiff::Error;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		value.parse().map(Self)
	}
}

impl Display for Duration {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.0, f)
	}
}

impl PartialEq for Duration {
	fn eq(&self, other: &Self) -> bool {
		self.ordering_key() == other.ordering_key()
	}
}

impl Eq for Duration {}

impl PartialOrd for Duration {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Duration {
	fn cmp(&self, other: &Self) -> Ordering {
		self.ordering_key().cmp(&other.ordering_key())
	}
}

impl Hash for Duration {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.ordering_key().hash(state);
	}
}

#[cfg(test)]
mod tests {
	use super::{DateTime, Duration};

	#[test]
	fn datetime_equality_uses_instant() {
		let utc = "2025-01-01T00:00:00+00:00[UTC]".parse::<DateTime>().unwrap();
		let new_york = "2024-12-31T19:00:00-05:00[America/New_York]".parse::<DateTime>().unwrap();
		assert_eq!(utc, new_york);
	}

	#[test]
	fn duration_normalizes_equivalent_units() {
		let year = "P1Y".parse::<Duration>().unwrap();
		let months = "P12M".parse::<Duration>().unwrap();
		let week = "P1W".parse::<Duration>().unwrap();
		let days = "P7D".parse::<Duration>().unwrap();
		assert_eq!(year, months);
		assert_eq!(week, days);
	}
}
