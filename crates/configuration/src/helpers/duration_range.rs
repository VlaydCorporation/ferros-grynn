use crate::{Error, Result};
use humantime::{format_duration, parse_duration};
use itertools::Itertools;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::time::Duration;

pub struct DurationRange {
	pub min: Duration,
	pub max: Duration,
}

impl DurationRange {
	pub fn new(min: Duration, max: Duration) -> DurationRange {
		DurationRange {
			min: std::cmp::min(min, max),
			max: std::cmp::max(min, max),
		}
	}

	pub fn parse(value: &str) -> Result<DurationRange> {
		let binding = value.trim().replace(r"^\\[", "").replace(r"\\]$", "");
		let parts = binding.split("-").collect_vec();
		if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
			Err(Error::InvalidFormat(
				"must be in format <min>-<max>, where min and max are non-negative durations"
					.to_string(),
			))
		} else {
			Ok(DurationRange {
				min: parse_duration(parts[0])?,
				max: parse_duration(parts[1])?,
			})
		}
	}

	pub fn delta(&self) -> Duration {
		self.max - self.min
	}

	pub fn value_to_string(&self) -> String {
		format!("{}-{}", format_duration(self.min), format_duration(self.max))
	}
}

impl PartialEq for DurationRange {
	fn eq(&self, other: &Self) -> bool {
		self.min == other.min && self.max == other.max
	}

	fn ne(&self, other: &Self) -> bool {
		self.min != other.min || self.max != other.max
	}
}

impl Hash for DurationRange {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.min.hash(state);
		self.max.hash(state);
	}
}

impl Display for DurationRange {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "[{}-{}]", format_duration_iso8601(self.min), format_duration_iso8601(self.max))
	}
}

fn format_duration_iso8601(duration: Duration) -> String {
	let secs_total = duration.as_secs();
	let nanos = duration.subsec_nanos();

	if secs_total == 0 && nanos == 0 {
		return "PT0S".to_string();
	}

	let hours = secs_total / 3600;
	let minutes = (secs_total % 3600) / 60;
	let seconds = secs_total % 60;

	let mut result = "PT".to_string();

	if hours > 0 {
		result.push_str(&format!("{}H", hours));
	}
	if minutes > 0 {
		result.push_str(&format!("{}M", minutes));
	}

	if seconds > 0 || nanos > 0 {
		if nanos == 0 {
			result.push_str(&format!("{}S", seconds));
		} else {
			// Formatting nanoseconds into fractions of a second, removing extra zeros.
			let fractional = format!(".{:09}", nanos).trim_end_matches('0').to_string();
			result.push_str(&format!("{}{}", seconds, fractional));
			result.push('S');
		}
	}

	result
}
