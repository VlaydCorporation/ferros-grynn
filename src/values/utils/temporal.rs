use crate::gql_status::FerrosGrynnResult;
use crate::math::{AddExact, MultiplyExact};

pub const NANOS_PER_SECOND: i64 = 1_000_000_000;
pub const AVG_NANOS_PER_MONTH: i64 = 2_629_746_000_000_000;
pub const SECONDS_PER_DAY: i64 = ChronoUnit::Days.duration().as_secs();
pub const AVG_SECONDS_PER_MONTH: i64 = 2_629_746;

pub fn seconds_with_nanos(seconds: i64, nanos: i64) -> FerrosGrynnResult<i64> {
	seconds.add_exact(&(nanos / NANOS_PER_SECOND))
}

pub fn average_length_in_seconds(months: i64, days: i64, seconds: i64) -> FerrosGrynnResult<i64> {
	let days_in_secs = days.multiply_exact(&SECONDS_PER_DAY)?;
	let months_in_secs = months.multiply_exact(&AVG_SECONDS_PER_MONTH)?;
	seconds.add_exact(&days_in_secs.add_exact(&months_in_secs)?)
}
