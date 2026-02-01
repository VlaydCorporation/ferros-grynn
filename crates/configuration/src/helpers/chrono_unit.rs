use chrono::Duration as ChronoDuration;
use std::time::Duration as StdDuration;
use magic_utils::Display;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display)]
pub enum ChronoUnit {
	Nanos,
	Micros,
	Millis,
	Seconds,
	Minutes,
	Hours,
	Days,
}

impl ChronoUnit {
	pub fn to_chrono_duration(self) -> ChronoDuration {
		match self {
			ChronoUnit::Nanos => ChronoDuration::nanoseconds(1),
			ChronoUnit::Micros => ChronoDuration::microseconds(1),
			ChronoUnit::Millis => ChronoDuration::milliseconds(1),
			ChronoUnit::Seconds => ChronoDuration::seconds(1),
			ChronoUnit::Minutes => ChronoDuration::minutes(1),
			ChronoUnit::Hours => ChronoDuration::hours(1),
			ChronoUnit::Days => ChronoDuration::days(1),
		}
	}

	pub fn to_std_duration(self) -> StdDuration {
		match self {
			ChronoUnit::Nanos => StdDuration::from_nanos(1),
			ChronoUnit::Micros => StdDuration::from_micros(1),
			ChronoUnit::Millis => StdDuration::from_millis(1),
			ChronoUnit::Seconds => StdDuration::from_secs(1),
			ChronoUnit::Minutes => StdDuration::from_secs(60),
			ChronoUnit::Hours => StdDuration::from_secs(3600),
			ChronoUnit::Days => StdDuration::from_secs(86400),
		}
	}
}

pub trait Truncatable {
	fn truncated_to(&self, unit: &ChronoUnit) -> Self;

	fn resolution(&self) -> ChronoUnit;
}

impl Truncatable for ChronoDuration {
	fn truncated_to(&self, unit: &ChronoUnit) -> Self {
		let unit_duration = unit.to_chrono_duration();
		let total_nanos = self.num_nanoseconds().unwrap_or(0); // Handle potential overflow/underflow if duration is too large
		let unit_nanos = unit_duration.num_nanoseconds().unwrap_or(1); // Should not be 0 for valid units

		if unit_nanos == 0 {
			// This case should ideally not happen with valid ChronoUnit definitions
			return ChronoDuration::zero();
		}

		let truncated_nanos = (total_nanos / unit_nanos) * unit_nanos;
		ChronoDuration::nanoseconds(truncated_nanos)
	}

	fn resolution(&self) -> ChronoUnit {
		let total_ns = self.num_nanoseconds().unwrap_or(0).abs();
		if total_ns == 0 {
			return ChronoUnit::Nanos;
		}

		if total_ns % 1_000 != 0 {
			ChronoUnit::Nanos
		} else if (total_ns / 1_000) % 1_000 != 0 {
			ChronoUnit::Micros
		} else if (total_ns / 1_000_000) % 60 != 0 {
			ChronoUnit::Millis
		} else if (total_ns / 1_000_000_000) % 60 != 0 {
			ChronoUnit::Seconds
		} else if (total_ns / (60 * 1_000_000_000)) % 60 != 0 {
			ChronoUnit::Minutes
		} else if (total_ns / (3600 * 1_000_000_000)) % 24 != 0 {
			ChronoUnit::Hours
		} else {
			ChronoUnit::Days
		}
	}
}

impl Truncatable for StdDuration {
	fn truncated_to(&self, unit: &ChronoUnit) -> Self {
		let total_nanos = self.as_nanos();
		let truncated_nanos = match unit {
			ChronoUnit::Nanos => total_nanos,
			ChronoUnit::Micros => (total_nanos / 1_000) * 1_000,
			ChronoUnit::Millis => (total_nanos / 1_000_000) * 1_000_000,
			ChronoUnit::Seconds => (total_nanos / 1_000_000_000) * 1_000_000_000,
			ChronoUnit::Minutes => (total_nanos / (60 * 1_000_000_000)) * (60 * 1_000_000_000),
			ChronoUnit::Hours => (total_nanos / (3600 * 1_000_000_000)) * (3600 * 1_000_000_000),
			ChronoUnit::Days => {
				// StdDuration does not directly support days, so we calculate nanos in a day
				// and then truncate.
				let nanos_in_day = 24 * 3600 * 1_000_000_000;
				(total_nanos / nanos_in_day) * nanos_in_day
			}
		};
		StdDuration::from_nanos(truncated_nanos as u64)
	}

	fn resolution(&self) -> ChronoUnit {
		let nanos_part = self.subsec_nanos();
		if nanos_part == 0 {
			// For a zero duration, we can assume the highest possible resolution.
			return ChronoUnit::Nanos;
		}

		let secs_part = self.as_secs();

		if nanos_part % 1_000 != 0 {
			ChronoUnit::Nanos
		} else if (nanos_part / 1_000) % 1_000 != 0 {
			ChronoUnit::Micros
		} else if nanos_part / 1_000_000 > 0 {
			ChronoUnit::Millis
		} else if secs_part % 60 != 0 {
			ChronoUnit::Seconds
		} else if (secs_part / 60) % 60 != 0 {
			ChronoUnit::Minutes
		} else if (secs_part / 3600) % 24 != 0 {
			ChronoUnit::Hours
		} else {
			ChronoUnit::Days
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_truncate() {
		let std_dur = StdDuration::from_micros(1_234_567); // 1.234567 секунд
		println!("Оригинальный std::time::Duration: {:?}", std_dur);

		let std_dur_truncated = std_dur.truncated_to(&ChronoUnit::Millis);
		println!("Усечено до миллисекунд: {}", std_dur_truncated.as_millis());
		assert_eq!(std_dur_truncated, StdDuration::from_millis(1234));

		let std_dur_truncated_s = std_dur.truncated_to(&ChronoUnit::Seconds);
		println!("Усечено до секунд: {}", std_dur_truncated_s.as_secs());
		assert_eq!(std_dur_truncated_s, StdDuration::from_secs(1));

		println!("---");

		// Пример для chrono::Duration
		let chrono_dur = ChronoDuration::nanoseconds(1_234_567_890); // 1.234567890 секунд
		println!("Оригинальный chrono::Duration: {}", chrono_dur);

		let chrono_dur_truncated = chrono_dur.truncated_to(&ChronoUnit::Millis);
		println!("Усечено до миллисекунд: {}", chrono_dur_truncated.num_milliseconds());
		assert_eq!(chrono_dur_truncated, ChronoDuration::milliseconds(1234));

		let chrono_dur_truncated_s = chrono_dur.truncated_to(&ChronoUnit::Seconds);
		println!("Усечено до секунд: {}", chrono_dur_truncated_s.num_seconds());
		assert_eq!(chrono_dur_truncated_s, ChronoDuration::seconds(1));
	}

	#[test]
	fn test_resolution() {
		let std_duration_seconds = StdDuration::from_secs(120);
		let std_duration_ms = StdDuration::from_millis(5000);
		let std_duration_complex = StdDuration::new(10, 1_234_567);

		println!(
			"Resolution of {:?}: {:?}",
			std_duration_seconds,
			std_duration_seconds.resolution()
		);
		assert_eq!(std_duration_seconds.resolution(), ChronoUnit::Minutes);
		println!("Resolution of {:?}: {:?}", std_duration_ms, std_duration_ms.resolution());
		assert_eq!(std_duration_ms.resolution(), ChronoUnit::Seconds);
		println!(
			"Resolution of {:?}: {:?}",
			std_duration_complex,
			std_duration_complex.resolution()
		);
		assert_eq!(std_duration_complex.resolution(), ChronoUnit::Nanos);

		println!("---");

		let chrono_duration_hours = ChronoDuration::hours(48);
		let chrono_duration_minutes = ChronoDuration::minutes(180);
		let chrono_duration_micros = ChronoDuration::microseconds(123_456);
		let chrono_duration_complex = ChronoDuration::nanoseconds(123_456_789_123_456);

		println!(
			"Resolution of {}: {:?}",
			chrono_duration_hours,
			chrono_duration_hours.resolution()
		);
		assert_eq!(chrono_duration_hours.resolution(), ChronoUnit::Days);
		println!(
			"Resolution of {}: {:?}",
			chrono_duration_minutes,
			chrono_duration_minutes.resolution()
		);
		assert_eq!(chrono_duration_minutes.resolution(), ChronoUnit::Hours);
		println!(
			"Resolution of {}: {:?}",
			chrono_duration_micros,
			chrono_duration_micros.resolution()
		);
		assert_eq!(chrono_duration_micros.resolution(), ChronoUnit::Micros);
		println!(
			"Resolution of {}: {:?}",
			chrono_duration_complex,
			chrono_duration_complex.resolution()
		);
		assert_eq!(chrono_duration_complex.resolution(), ChronoUnit::Nanos);
	}
}
