// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wall clock and UTC calendar dates.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds in one day.
pub const SECONDS_PER_DAY: i64 = 86_400;
const MILLIS_PER_SECOND: u64 = 1000;
const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 3600;
/// Days in one 400-year cycle of the Gregorian calendar.
const DAYS_PER_ERA: i64 = 146_097;
const YEARS_PER_ERA: i64 = 400;
/// Days from 0000-03-01 to 1970-01-01.
const EPOCH_SHIFT_DAYS: i64 = 719_468;
const MONTHS_PER_YEAR: u32 = 12;
const DAYS_PER_YEAR: i64 = 365;
/// Days in 4 years with their leap day, and the year step of leap days.
const DAYS_PER_4_YEARS: i64 = 1460;
const YEARS_PER_LEAP: i64 = 4;
/// Days in 100 years (24 leap days) and the year step of skipped leap days.
const DAYS_PER_CENTURY: i64 = 36_524;
const YEARS_PER_CENTURY: i64 = 100;
/// The last day of an era, counted from 0.
const LAST_DAY_OF_ERA: i64 = 146_096;
/// From March, months follow a 5-month pattern of 153 days (31 30 31 30 31).
const DAYS_PER_MONTH_PATTERN: i64 = 153;
const MONTHS_PER_PATTERN: i64 = 5;
/// Rounds the month pattern so each month starts on its first day.
const MONTH_PATTERN_ROUNDING: i64 = 2;
/// Months from March to December: month indexes below it are in the same calendar year.
const MONTHS_MARCH_TO_DECEMBER: i64 = 10;
/// The calendar below counts years from March, so January and February belong to the year
/// before; these are the month shifts between the two counts.
const MARCH_SHIFT: u32 = 3;
const JANUARY_SHIFT: u32 = 9;
const SEPARATOR: char = '-';
/// Between the date and the time of day in a date-time text (`2027-01-31T00:00:00`).
const DATE_TIME_SEPARATOR: char = 'T';
/// Between the hours, minutes and seconds of a time of day.
const TIME_SEPARATOR: char = ':';
const HOURS_PER_DAY: u64 = 24;

/// Milliseconds since 1970-01-01 UTC; 0 for a clock before that.
pub fn unix_ms() -> u64 {
    unix_ms_at(SystemTime::now())
}

/// Seconds since 1970-01-01 UTC; 0 for a clock before that.
pub fn unix_seconds() -> i64 {
    unix_seconds_at(SystemTime::now())
}

fn unix_seconds_at(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
    })
}

fn unix_ms_at(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// A UTC day of the Gregorian calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CivilDate {
    /// The year, for example 2026.
    pub year: i64,
    /// The month, 1 to 12.
    pub month: u32,
    /// The day of the month, 1 to 31.
    pub day: u32,
}

impl CivilDate {
    /// The day `days` days after 1970-01-01.
    pub fn from_days(days: i64) -> Self {
        // H. Hinnant's `civil_from_days`, with years that start in March.
        let shifted = days + EPOCH_SHIFT_DAYS;
        let era = shifted.div_euclid(DAYS_PER_ERA);
        let day_of_era = shifted.rem_euclid(DAYS_PER_ERA);
        let year_of_era = (day_of_era - day_of_era / DAYS_PER_4_YEARS
            + day_of_era / DAYS_PER_CENTURY
            - day_of_era / LAST_DAY_OF_ERA)
            / DAYS_PER_YEAR;
        let day_of_year = day_of_era
            - (DAYS_PER_YEAR * year_of_era + year_of_era / YEARS_PER_LEAP
                - year_of_era / YEARS_PER_CENTURY);
        let month_index =
            (MONTHS_PER_PATTERN * day_of_year + MONTH_PATTERN_ROUNDING) / DAYS_PER_MONTH_PATTERN;
        let first_of_month =
            (DAYS_PER_MONTH_PATTERN * month_index + MONTH_PATTERN_ROUNDING) / MONTHS_PER_PATTERN;
        let day = u32::try_from(day_of_year - first_of_month + 1).unwrap_or(1);
        let march_based = month_index < MONTHS_MARCH_TO_DECEMBER;
        let month_index = u32::try_from(month_index).unwrap_or(0);
        let month = if march_based {
            month_index + MARCH_SHIFT
        } else {
            month_index - JANUARY_SHIFT
        };
        let year = year_of_era + era * YEARS_PER_ERA + i64::from(month <= 2);
        Self { year, month, day }
    }

    /// The days from 1970-01-01 to this day (negative before it).
    pub fn to_days(self) -> i64 {
        // H. Hinnant's `days_from_civil`.
        let year = self.year - i64::from(self.month <= 2);
        let era = year.div_euclid(YEARS_PER_ERA);
        let year_of_era = year.rem_euclid(YEARS_PER_ERA);
        let month_index = if self.month > 2 {
            self.month - MARCH_SHIFT
        } else {
            self.month + JANUARY_SHIFT
        };
        let first_of_month = (DAYS_PER_MONTH_PATTERN * i64::from(month_index)
            + MONTH_PATTERN_ROUNDING)
            / MONTHS_PER_PATTERN;
        let day_of_year = first_of_month + i64::from(self.day) - 1;
        let day_of_era = year_of_era * DAYS_PER_YEAR + year_of_era / YEARS_PER_LEAP
            - year_of_era / YEARS_PER_CENTURY
            + day_of_year;
        era * DAYS_PER_ERA + day_of_era - EPOCH_SHIFT_DAYS
    }

    /// The day of `unix_seconds` (seconds since 1970-01-01 UTC).
    pub fn of_unix_seconds(unix_seconds: i64) -> Self {
        Self::from_days(unix_seconds.div_euclid(SECONDS_PER_DAY))
    }

    /// Reads `year-month-day` with decimal numbers, as the client reads its date keys
    /// (`%d-%d-%d`); `None` for other text or a day the calendar does not have.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().splitn(3, SEPARATOR);
        let year = parts.next()?.parse().ok()?;
        let month = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        let date = Self { year, month, day };
        (Self::from_days(date.to_days()) == date).then_some(date)
    }

    /// The months from January of year 0 to this month: two dates' difference is the number of
    /// month starts between them.
    pub fn month_number(self) -> i64 {
        self.year * i64::from(MONTHS_PER_YEAR) + i64::from(self.month)
    }
}

/// The Unix seconds of the UTC date-time text `year-month-dayThh:mm:ss`, as the client's season
/// keys write it (`javelin.leaderboard-season-end-datetime` `2027-01-31T00:00:00`); `None` for
/// other text or a time that the day does not have.
pub fn unix_seconds_of_date_time(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once(DATE_TIME_SEPARATOR)?;
    let date = CivilDate::parse(date)?;
    let mut parts = time.splitn(3, TIME_SEPARATOR);
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds: u64 = parts.next()?.parse().ok()?;
    if hours >= HOURS_PER_DAY || minutes >= SECONDS_PER_MINUTE || seconds >= SECONDS_PER_MINUTE {
        return None;
    }
    let seconds_of_day = hours * SECONDS_PER_HOUR + minutes * SECONDS_PER_MINUTE + seconds;
    Some(date.to_days() * SECONDS_PER_DAY + i64::try_from(seconds_of_day).ok()?)
}

/// ISO 8601 UTC text with milliseconds, for example `2026-06-30T09:53:59.123Z`.
pub fn iso8601_millis(unix_ms: u64) -> String {
    let (date, seconds_of_day) = date_and_seconds(unix_ms / MILLIS_PER_SECOND);
    format!(
        "{}.{:03}Z",
        date_time_text(date, seconds_of_day),
        unix_ms % MILLIS_PER_SECOND
    )
}

/// ISO 8601 UTC text to the second, for example `2026-06-30T10:08:56Z`.
pub fn iso8601_seconds(unix_seconds: u64) -> String {
    let (date, seconds_of_day) = date_and_seconds(unix_seconds);
    format!("{}Z", date_time_text(date, seconds_of_day))
}

fn date_and_seconds(unix_seconds: u64) -> (CivilDate, u64) {
    let seconds = i64::try_from(unix_seconds).unwrap_or(i64::MAX);
    let date = CivilDate::of_unix_seconds(seconds);
    let seconds_of_day = u64::try_from(seconds.rem_euclid(SECONDS_PER_DAY)).unwrap_or(0);
    (date, seconds_of_day)
}

fn date_time_text(date: CivilDate, seconds_of_day: u64) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        date.year,
        date.month,
        date.day,
        seconds_of_day / SECONDS_PER_HOUR,
        seconds_of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
        seconds_of_day % SECONDS_PER_MINUTE
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const SECOND_MS: u64 = 1000;
    const SECONDS: u64 = 90;
    const SECONDS_I64: i64 = 90;

    #[test]
    fn seconds_count_from_1970_and_earlier_times_are_0() {
        assert_eq!(
            unix_seconds_at(UNIX_EPOCH + Duration::from_secs(SECONDS)),
            SECONDS_I64
        );
        assert_eq!(unix_seconds_at(UNIX_EPOCH - Duration::from_secs(1)), 0);
        assert!(unix_seconds() > 0);
    }

    #[test]
    fn times_count_from_1970_and_earlier_times_are_0() {
        assert_eq!(unix_ms_at(UNIX_EPOCH + Duration::from_secs(1)), SECOND_MS);
        assert_eq!(unix_ms_at(UNIX_EPOCH - Duration::from_secs(1)), 0);
        assert!(unix_ms() > 0);
    }

    /// 2023-02-07, the retail `javelin.leaderboard-rotation-startDate`.
    const ROTATION_START: CivilDate = CivilDate {
        year: 2023,
        month: 2,
        day: 7,
    };
    const ROTATION_START_DAYS: i64 = 19_395;
    /// 2000-02-29 and the day after.
    const LEAP_DAY: CivilDate = CivilDate {
        year: 2000,
        month: 2,
        day: 29,
    };
    const LEAP_DAY_DAYS: i64 = 11_016;
    /// 2026-06-30 09:53:59.123 UTC.
    const SAMPLE_MS: u64 = 1_782_813_239_123;

    #[test]
    fn days_and_dates_convert_both_ways() {
        assert_eq!(ROTATION_START.to_days(), ROTATION_START_DAYS);
        assert_eq!(CivilDate::from_days(ROTATION_START_DAYS), ROTATION_START);
        assert_eq!(CivilDate::from_days(LEAP_DAY_DAYS), LEAP_DAY);
        assert_eq!(
            CivilDate::from_days(LEAP_DAY_DAYS + 1),
            CivilDate {
                year: 2000,
                month: 3,
                day: 1
            }
        );
        assert_eq!(
            CivilDate::from_days(-1),
            CivilDate {
                year: 1969,
                month: 12,
                day: 31
            }
        );
        for days in -800_000..800_000 {
            assert_eq!(CivilDate::from_days(days).to_days(), days);
        }
    }

    #[test]
    fn dates_parse_like_the_client() {
        assert_eq!(CivilDate::parse("2023-02-07"), Some(ROTATION_START));
        assert_eq!(CivilDate::parse("2023-2-7"), Some(ROTATION_START));
        for bad in ["2023-02-30", "2023-13-01", "2023/02/07", "2023-02", ""] {
            assert_eq!(CivilDate::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn month_numbers_count_month_starts() {
        let june_2026 = CivilDate {
            year: 2026,
            month: 6,
            day: 30,
        };
        assert_eq!(june_2026.month_number() - ROTATION_START.month_number(), 40);
    }

    /// 2027-01-31 00:00:00 UTC, the retail season end.
    const SEASON_END_SECONDS: i64 = 1_801_353_600;

    #[test]
    fn date_times_parse_as_utc() {
        assert_eq!(
            unix_seconds_of_date_time("2027-01-31T00:00:00"),
            Some(SEASON_END_SECONDS)
        );
        assert_eq!(
            unix_seconds_of_date_time("2026-06-30T09:53:59"),
            Some(i64::try_from(SAMPLE_MS / SECOND_MS).expect("seconds"))
        );
        for bad in [
            "2026-06-30-T17:00:0",
            "2027-01-31",
            "2027-01-31T24:00:00",
            "2027-01-31T00:60:00",
            "2027-02-30T00:00:00",
        ] {
            assert_eq!(unix_seconds_of_date_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn iso_texts_are_utc() {
        assert_eq!(iso8601_millis(SAMPLE_MS), "2026-06-30T09:53:59.123Z");
        assert_eq!(iso8601_seconds(SAMPLE_MS / 1000), "2026-06-30T09:53:59Z");
        assert_eq!(iso8601_millis(0), "1970-01-01T00:00:00.000Z");
    }
}
