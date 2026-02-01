use crate::gql_status::FerrosGrynnResult;
use crate::math::{AddExact, MultiplyExact};
use jiff::SignedDuration;
use magic_utils::Display;

//#region ----------------- TRAITS -----------------
pub trait Temporal {
    fn supports_field(&self, field: &ChronoField) -> bool;
    fn supports_unit(&self, unit: &ChronoUnit) -> bool;

    fn range(&self, field: &ChronoField) -> Option<ChronoRange> {
        if self.supports_field(field) {
            Some(field.range())
        } else {
            None
        }
    }

    fn until(&self, end_exclusive: &impl Temporal, unit: &ChronoUnit) -> i64;
}
//#endregion ----------------- TRAITS -----------------

//#region ----------------- UNIT, FIELD and RANGE -----------------
#[derive(Display, Ord, PartialOrd, Eq, PartialEq)]
#[repr(u8)]
pub enum ChronoUnit {
    Nanos,
    Micros,
    Millis,
    Seconds,
    Minutes,
    Hours,
    HalfDays,
    Days,
    Weeks,
    Months,
    Years,
    Decades,
    Centuries,
    Millennia,
    Eras,
    Forever,
}

impl ChronoUnit {
    pub const fn duration(&self) -> SignedDuration {
        match self {
            ChronoUnit::Nanos => SignedDuration::from_nanos(1),
            ChronoUnit::Micros => SignedDuration::from_nanos(1_000),
            ChronoUnit::Millis => SignedDuration::from_nanos(1_000_000),
            ChronoUnit::Seconds => SignedDuration::from_secs(1),
            ChronoUnit::Minutes => SignedDuration::from_secs(60),
            ChronoUnit::Hours => SignedDuration::from_secs(3600),
            ChronoUnit::HalfDays => SignedDuration::from_secs(43200),
            ChronoUnit::Days => SignedDuration::from_secs(86400),
            ChronoUnit::Weeks => SignedDuration::from_secs(7 * 86400),
            ChronoUnit::Months => SignedDuration::from_secs(31556952 / 12),
            ChronoUnit::Years => SignedDuration::from_secs(31556952),
            ChronoUnit::Decades => SignedDuration::from_secs(31556952 * 10),
            ChronoUnit::Centuries => SignedDuration::from_secs(31556952 * 100),
            ChronoUnit::Millennia => SignedDuration::from_secs(31556952 * 1000),
            ChronoUnit::Eras => SignedDuration::from_secs(31556952 * 1000_000_000),
            ChronoUnit::Forever => SignedDuration::MAX,
        }
    }

    pub fn is_duration_estimated(&self) -> bool {
        self >= &ChronoUnit::Days
    }

    pub fn is_date_based(&self) -> bool {
        self >= &ChronoUnit::Days && self != &ChronoUnit::Forever
    }

    pub fn is_time_based(&self) -> bool {
        self < &ChronoUnit::Days
    }

    pub fn between(&self, inclusive1: &impl Temporal, inclusive2: &impl Temporal) -> i64 {
        inclusive1.until(inclusive2, self)
    }
}

#[derive(Display, Ord, PartialOrd, Eq, PartialEq)]
#[repr(u8)]
pub enum ChronoField {
    NanoOfSecond,
    NanoOfDay,
    MicroOfSecond,
    MicroOfDay,
    MilliOfSecond,
    MilliOfDay,
    SecondsOfMinute,
    SecondsOfDay,
    MinuteOfHour,
    MinuteOfDay,
    HourOfAMPM,
    ClockHourOfAMPM,
    HourOfDay,
    ClockHourOfDay,
    AMPMOfDay,
    DayOfWeek,
    AlignedDayOfWeekInMonth,
    AlignedDayOfWeekInYear,
    DayOfMonth,
    DayOfYear,
    EpochDay,
    AlignedWeekOfMonth,
    AlignedWeekOfYear,
    MonthOfYear,
    ProlepticMonth,
    YearOfDecade,
    YearOfCentury,
    YearOfMillennium,
    YearOfEra,
    Year,
    Era,
    InstantSeconds,
    OffsetSeconds,
}

impl ChronoField {
    pub const fn base_unit(&self) -> ChronoUnit {
        match self {
            ChronoField::NanoOfSecond => ChronoUnit::Nanos,
            ChronoField::NanoOfDay => ChronoUnit::Nanos,
            ChronoField::MicroOfSecond => ChronoUnit::Micros,
            ChronoField::MicroOfDay => ChronoUnit::Micros,
            ChronoField::MilliOfSecond => ChronoUnit::Millis,
            ChronoField::MilliOfDay => ChronoUnit::Millis,
            ChronoField::SecondsOfMinute => ChronoUnit::Seconds,
            ChronoField::SecondsOfDay => ChronoUnit::Seconds,
            ChronoField::MinuteOfHour => ChronoUnit::Minutes,
            ChronoField::MinuteOfDay => ChronoUnit::Minutes,
            ChronoField::HourOfAMPM => ChronoUnit::Hours,
            ChronoField::ClockHourOfAMPM => ChronoUnit::Hours,
            ChronoField::HourOfDay => ChronoUnit::Hours,
            ChronoField::ClockHourOfDay => ChronoUnit::Hours,
            ChronoField::AMPMOfDay => ChronoUnit::HalfDays,
            ChronoField::DayOfWeek => ChronoUnit::Days,
            ChronoField::AlignedDayOfWeekInMonth => ChronoUnit::Days,
            ChronoField::AlignedDayOfWeekInYear => ChronoUnit::Days,
            ChronoField::DayOfMonth => ChronoUnit::Days,
            ChronoField::DayOfYear => ChronoUnit::Days,
            ChronoField::EpochDay => ChronoUnit::Days,
            ChronoField::AlignedWeekOfMonth => ChronoUnit::Weeks,
            ChronoField::AlignedWeekOfYear => ChronoUnit::Weeks,
            ChronoField::MonthOfYear => ChronoUnit::Months,
            ChronoField::ProlepticMonth => ChronoUnit::Months,
            ChronoField::YearOfDecade => ChronoUnit::Years,
            ChronoField::YearOfCentury => ChronoUnit::Years,
            ChronoField::YearOfMillennium => ChronoUnit::Years,
            ChronoField::YearOfEra => ChronoUnit::Years,
            ChronoField::Year => ChronoUnit::Years,
            ChronoField::Era => ChronoUnit::Eras,
            ChronoField::InstantSeconds => ChronoUnit::Seconds,
            ChronoField::OffsetSeconds => ChronoUnit::Seconds,
        }
    }

    pub const fn range_unit(&self) -> ChronoUnit {
        match self {
            ChronoField::NanoOfSecond => ChronoUnit::Seconds,
            ChronoField::NanoOfDay => ChronoUnit::Days,
            ChronoField::MicroOfSecond => ChronoUnit::Seconds,
            ChronoField::MicroOfDay => ChronoUnit::Days,
            ChronoField::MilliOfSecond => ChronoUnit::Seconds,
            ChronoField::MilliOfDay => ChronoUnit::Days,
            ChronoField::SecondsOfMinute => ChronoUnit::Minutes,
            ChronoField::SecondsOfDay => ChronoUnit::Days,
            ChronoField::MinuteOfHour => ChronoUnit::Hours,
            ChronoField::MinuteOfDay => ChronoUnit::Days,
            ChronoField::HourOfAMPM => ChronoUnit::HalfDays,
            ChronoField::ClockHourOfAMPM => ChronoUnit::HalfDays,
            ChronoField::HourOfDay => ChronoUnit::Days,
            ChronoField::ClockHourOfDay => ChronoUnit::Days,
            ChronoField::AMPMOfDay => ChronoUnit::Days,
            ChronoField::DayOfWeek => ChronoUnit::Weeks,
            ChronoField::AlignedDayOfWeekInMonth => ChronoUnit::Weeks,
            ChronoField::AlignedDayOfWeekInYear => ChronoUnit::Weeks,
            ChronoField::DayOfMonth => ChronoUnit::Months,
            ChronoField::DayOfYear => ChronoUnit::Years,
            ChronoField::EpochDay => ChronoUnit::Forever,
            ChronoField::AlignedWeekOfMonth => ChronoUnit::Months,
            ChronoField::AlignedWeekOfYear => ChronoUnit::Years,
            ChronoField::MonthOfYear => ChronoUnit::Years,
            ChronoField::ProlepticMonth => ChronoUnit::Forever,
            ChronoField::YearOfDecade => ChronoUnit::Decades,
            ChronoField::YearOfCentury => ChronoUnit::Centuries,
            ChronoField::YearOfMillennium => ChronoUnit::Millennia,
            ChronoField::YearOfEra => ChronoUnit::Forever,
            ChronoField::Year => ChronoUnit::Forever,
            ChronoField::Era => ChronoUnit::Forever,
            ChronoField::InstantSeconds => ChronoUnit::Forever,
            ChronoField::OffsetSeconds => ChronoUnit::Forever,
        }
    }

    pub const fn range(&self) -> ChronoRange {
        match self {
            ChronoField::NanoOfSecond => ChronoRange::min_max(0, 999_999_999),
            ChronoField::NanoOfDay => ChronoRange::min_max(0, 86400 * 1000_000_000 - 1),
            ChronoField::MicroOfSecond => ChronoRange::min_max(0, 999_999),
            ChronoField::MicroOfDay => ChronoRange::min_max(0, 86400 * 1000_000 - 1),
            ChronoField::MilliOfSecond => ChronoRange::min_max(0, 999),
            ChronoField::MilliOfDay => ChronoRange::min_max(0, 86400 * 1000 - 1),
            ChronoField::SecondsOfMinute => ChronoRange::min_max(0, 59),
            ChronoField::SecondsOfDay => ChronoRange::min_max(0, 86400 - 1),
            ChronoField::MinuteOfHour => ChronoRange::min_max(0, 59),
            ChronoField::MinuteOfDay => ChronoRange::min_max(0, 24 * 60 - 1),
            ChronoField::HourOfAMPM => ChronoRange::min_max(0, 1),
            ChronoField::ClockHourOfAMPM => ChronoRange::min_max(1, 12),
            ChronoField::HourOfDay => ChronoRange::min_max(0, 23),
            ChronoField::ClockHourOfDay => ChronoRange::min_max(1, 24),
            ChronoField::AMPMOfDay => ChronoRange::min_max(0, 1),
            ChronoField::DayOfWeek => ChronoRange::min_max(1, 7),
            ChronoField::AlignedDayOfWeekInMonth => ChronoRange::min_max(1, 7),
            ChronoField::AlignedDayOfWeekInYear => ChronoRange::min_max(1, 7),
            ChronoField::DayOfMonth => ChronoRange::min_two_max(1, 28, 31),
            ChronoField::DayOfYear => ChronoRange::min_two_max(1, 365, 366),
            ChronoField::EpochDay => ChronoRange::min_max(-365243219162, 365241780471),
            ChronoField::AlignedWeekOfMonth => ChronoRange::min_two_max(1, 4, 5),
            ChronoField::AlignedWeekOfYear => ChronoRange::min_max(1, 53),
            ChronoField::MonthOfYear => ChronoRange::min_max(1, 12),
            ChronoField::ProlepticMonth => {
                ChronoRange::min_max(-999_999_999 * 12, 999_999_999 * 12 + 11)
            }
            ChronoField::YearOfDecade => ChronoRange::min_max(-999_999_999 / 10, 999_999_999 / 10),
            ChronoField::YearOfCentury => ChronoRange::min_max(-999_999_999 / 100, 999_999_999 / 100),
            ChronoField::YearOfMillennium => ChronoRange::min_max(-999_999_999 / 1000, 999_999_999 / 1000),
            ChronoField::YearOfEra => ChronoRange::min_two_max(1, 999_999_999, 999_999_999 + 1),
            ChronoField::Year => ChronoRange::min_max(-999_999_999, 999_999_999),
            ChronoField::Era => ChronoRange::min_max(0, 1),
            ChronoField::InstantSeconds => {
                ChronoRange::min_max(-31557014167219200, 31556889864403199)
            }
            ChronoField::OffsetSeconds => ChronoRange::min_max(-18 * 3600, 18 * 3600),
        }
    }

    pub fn is_date_base(&self) -> bool {
        self >= &ChronoField::DayOfWeek && self <= &ChronoField::Era
    }

    pub fn is_time_base(&self) -> bool {
        self < &ChronoField::DayOfWeek
    }

    pub fn check(&self, value: i64) -> bool {
        self.range().check(value)
    }
}

pub struct ChronoRange {
    min_smallest: i64,
    min_largest: i64,
    max_smallest: i64,
    max_largest: i64,
}

impl ChronoRange {
    pub fn new(min_smallest: i64, min_largest: i64, max_smallest: i64, max_largest: i64) -> Self {
        Self {
            min_smallest,
            min_largest,
            max_smallest,
            max_largest,
        }
    }

    pub fn min_max(min: i64, max: i64) -> ChronoRange {
        Self {
            min_smallest: min,
            min_largest: min,
            max_smallest: max,
            max_largest: max,
        }
    }

    pub fn min_two_max(min: i64, max_smallest: i64, max_largest: i64) -> ChronoRange {
        Self {
            min_smallest: min,
            min_largest: min,
            max_smallest,
            max_largest,
        }
    }

    pub fn is_fixed(&self) -> bool {
        self.min_largest == self.min_largest && self.max_smallest == self.max_smallest
    }

    pub fn min(&self) -> i64 {
        self.min_smallest
    }

    pub fn min_largest(&self) -> i64 {
        self.min_largest
    }

    pub fn max(&self) -> i64 {
        self.max_smallest
    }

    pub fn max_smallest(&self) -> i64 {
        self.max_smallest
    }

    pub fn check(&self, value: i64) -> bool {
        value >= self.min() && value <= self.max()
    }
}
//#endregion ----------------- UNIT and FIELD -----------------