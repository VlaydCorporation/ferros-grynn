use crate::gql_status::{FerrosGrynnError, FerrosGrynnErrorBuilder, FerrosGrynnResult, Status};
use crate::math::SubtractExact;
use crate::values::MapValue;
use crate::values::storable::temporal::{ChronoField, ChronoUnit, Temporal};
use crate::values::utils::temporal::{
	NANOS_PER_SECOND, average_length_in_seconds, seconds_with_nanos,
};
use crate::values::utils::{
	AVG_NANOS_PER_MONTH, AccumulatorBuilder, SECONDS_PER_DAY, build_from_map, cast_to_i64,
	safe_cast_to_f64, safe_f64_to_i64,
};
use ferros_grynn_macros::StructureAccumulator;
use jiff::Span;

#[derive(StructureAccumulator)]
#[accum(years, weeks, hours, minutes, millis, micros)]
pub struct DurationValue {
	#[accum]
	months: i64,
	#[accum]
	days: i64,
	#[accum]
	seconds: i64,
	#[accum]
	nanos: i32,
}

impl DurationValue {
	fn new(months: i64, days: i64, seconds: i64, nanos: i64) -> FerrosGrynnResult<Self> {
		(|| -> FerrosGrynnResult<()> {
			// This is a mess really...
			// Since different values can have different signs (as allowed by Cypher & embedded)
			// We need to check all combinations of values for overflow

			// Nanos are normalized to [0, NANOS_PER_SEC-1], so first we check that seconds don't overflow
			let mut secs_with_nanos = seconds_with_nanos(seconds, nanos)?;
			if secs_with_nanos < 0 {
				secs_with_nanos = secs_with_nanos.subtract_exact(&1)?;
			}

			average_length_in_seconds(months, days, seconds)?;
			average_length_in_seconds(months, days, secs_with_nanos)?;
			Ok(())
		})()
		.map_err(|err| invalid_duration(months, days, seconds, nanos, err))?;

		let mut seconds = seconds_with_nanos(seconds, nanos)?;
		let mut nanos = nanos % NANOS_PER_SECOND;
		// normalize nanos to be between 0 and NANOS_PER_SECOND-1
		if nanos < 0 {
			seconds -= 1;
			nanos += NANOS_PER_SECOND;
		}
		let nanos = nanos as i32;

		Ok(Self {
			months,
			days,
			seconds,
			nanos,
		})
	}

	pub fn duration(months: i64, days: i64, seconds: i64, nanos: i64) -> FerrosGrynnResult<Self> {
		Self::new(months, days, seconds, nanos)
	}

	pub fn approximate(month: f64, days: f64, seconds: f64, nanos: f64) -> FerrosGrynnResult<Self> {
		// Do not use .fract(). `safe_f64_to_i64` checks overflows.

		let nanos_per_day = (NANOS_PER_SECOND * SECONDS_PER_DAY) as f64;

		let month_i64 = safe_f64_to_i64("month", month)?;

		let month_diff = AVG_NANOS_PER_MONTH as f64 * (month - month_i64 as f64);
		let days_i64 = safe_f64_to_i64("days", days + month_diff / nanos_per_day)?;

		let days_diff = nanos_per_day * (days - days_i64 as f64);
		let seconds_i64 =
			safe_f64_to_i64("seconds", seconds + days_diff / NANOS_PER_SECOND as f64)?;

		let seconds_diff = NANOS_PER_SECOND as f64 * (seconds - seconds_i64 as f64);
		let nanos_i64 = safe_f64_to_i64("nanos", nanos + seconds_diff)?;

		DurationValue::duration(month_i64, days_i64, seconds_i64, nanos_i64)
	}

	pub fn parse<S: AsRef<str>>(text: &S) -> FerrosGrynnResult<Self> {
		let span: Span = text.as_ref().parse()?;
		span.try_into()
	}

	pub fn build(map: &MapValue) -> FerrosGrynnResult<Self> {
		build_from_map::<DurationValueAccumulator>(map)
	}

	pub fn between(
		mut from: impl Temporal,
		mut to: impl Temporal,
		unit: Option<ChronoUnit>,
	) -> FerrosGrynnResult<Self> {
		if let Some(unit) = unit {
		} else {
			let mut month = 0;
			let mut days = 0;

			if from.supports_field(&ChronoField::EpochDay) && to.supports_field(&ChronoField::EpochDay) {
				if let Some(until) = from.until(&to, &ChronoUnit::Months) {

				}
			}

		}
	}

	pub fn to_span(&self) -> FerrosGrynnResult<Span> {
		Ok(Span::new()
			.try_months(self.months)?
			.try_days(self.days)?
			.try_seconds(self.seconds)?
			.try_nanoseconds(self.nanos)?)
	}
}

impl AccumulatorBuilder for DurationValueAccumulator {
	fn build(self) -> FerrosGrynnResult<Self::Output> {
		let all_integral = [
			&self.years,
			&self.months,
			&self.weeks,
			&self.days,
			&self.hours,
			&self.seconds,
			&self.millis,
			&self.micros,
			&self.nanos,
		]
		.iter()
		.all(|v| v.is_no_value() && v.is_integral());

		if all_integral {
			DurationValue::duration(
				cast_to_i64("years", self.years)? * 12 + cast_to_i64("months", self.months)?,
				cast_to_i64("weeks", self.weeks)? * 7 + cast_to_i64("days", self.days)?,
				cast_to_i64("hours", self.hours)? * 3600
					+ cast_to_i64("minutes", self.minutes)? * 60
					+ cast_to_i64("seconds", self.seconds)?,
				cast_to_i64("millis", self.millis)? * 1_000_000
					+ cast_to_i64("micros", self.micros)? * 1_000
					+ cast_to_i64("nanos", self.nanos)?,
			)
		} else {
			DurationValue::approximate(
				safe_cast_to_f64("years", self.years, 0.0)? * 12.0
					+ safe_cast_to_f64("months", self.months, 0.0)?,
				safe_cast_to_f64("weeks", self.weeks, 0.0)? * 7.0
					+ safe_cast_to_f64("days", self.days, 0.0)?,
				safe_cast_to_f64("hours", self.hours, 0.0)? * 3600.0
					+ safe_cast_to_f64("minutes", self.minutes, 0.0)? * 60.0
					+ safe_cast_to_f64("seconds", self.seconds, 0.0)?,
				safe_cast_to_f64("millis", self.millis, 0.0)? * 1_000_000.0
					+ safe_cast_to_f64("micros", self.micros, 0.0)? * 1_000.0
					+ safe_cast_to_f64("nanos", self.nanos, 0.0)?,
			)
		}
	}
}

impl TryFrom<Span> for DurationValue {
	type Error = FerrosGrynnError;
	fn try_from(span: Span) -> Result<Self, Self::Error> {
		let months = (span.get_years() as i64)
			.checked_mul(12)
			.ok_or(FerrosGrynnError::operation_overflow("*"))?
			.checked_add(span.get_months() as i64)
			.ok_or(FerrosGrynnError::operation_overflow("+"))?;

		let days = (span.get_weeks() as i64)
			.checked_mul(7)
			.ok_or(FerrosGrynnError::operation_overflow("*"))?
			.checked_add(span.get_days() as i64)
			.ok_or(FerrosGrynnError::operation_overflow("+"))?;

		let seconds = (span.get_hours() as i64)
			.checked_mul(3600)
			.ok_or(FerrosGrynnError::operation_overflow("*"))?
			.checked_add(
				span.get_minutes()
					.checked_mul(60)
					.ok_or(FerrosGrynnError::operation_overflow("*"))?,
			)
			.ok_or(FerrosGrynnError::operation_overflow("+"))?
			.checked_add(span.get_seconds())
			.ok_or(FerrosGrynnError::operation_overflow("+"))?;

		let nanos = span
			.get_milliseconds()
			.checked_mul(1_000_000)
			.ok_or(FerrosGrynnError::operation_overflow("*"))?
			.checked_add(
				span.get_microseconds()
					.checked_mul(1_000)
					.ok_or(FerrosGrynnError::operation_overflow("*"))?,
			)
			.ok_or(FerrosGrynnError::operation_overflow("+"))?
			.checked_add(span.get_nanoseconds())
			.ok_or(FerrosGrynnError::operation_overflow("+"))?;

		DurationValue::new(months, days, seconds, nanos)
	}
}

fn invalid_duration(
	months: i64,
	days: i64,
	seconds: i64,
	nanos: i64,
	cause: FerrosGrynnError,
) -> FerrosGrynnError {
	FerrosGrynnErrorBuilder::from_status(Status::InvalidArgument {
        msg: format!("Invalid value for duration, will cause overflow. Value was months={months}, days={days}, seconds={seconds}, nanos={nanos}"),
    })
        .with_cause(cause)
        .build()
}
