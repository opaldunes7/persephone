//! UTC calendar helpers for [`UnixNanos`] timestamps.

use crate::UnixNanos;

pub const NANOS_PER_MINUTE: u64 = 60_000_000_000;
pub const NANOS_PER_DAY: u64 = 86_400_000_000_000;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A UTC calendar date and time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

impl DateTime {
    #[must_use]
    pub fn from_nanos(ts: UnixNanos) -> Self {
        let (year, month, day) = civil_from_days((ts / NANOS_PER_DAY) as i64);
        let minutes = (ts % NANOS_PER_DAY) / NANOS_PER_MINUTE;
        Self {
            year,
            month,
            day,
            hour: (minutes / 60) as u32,
            minute: (minutes % 60) as u32,
        }
    }

    /// Three-letter English month name.
    #[must_use]
    pub fn month_name(&self) -> &'static str {
        MONTHS[(self.month - 1) as usize]
    }

    /// `YYYY-MM-DD`.
    #[must_use]
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `HH:MM`.
    #[must_use]
    pub fn time(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }
}

/// Days since 1970-01-01 → (year, month, day), from Howard Hinnant's `civil_from_days`.
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_utc_timestamps() {
        assert_eq!(DateTime::from_nanos(0).date(), "1970-01-01");
        let dt = DateTime::from_nanos(1_792_947_600_000_000_000);
        assert_eq!(
            (dt.date(), dt.time()),
            ("2026-10-25".into(), "17:00".into())
        );
        assert_eq!(dt.month_name(), "Oct");
        assert_eq!(
            DateTime::from_nanos(951_782_400_000_000_000).date(),
            "2000-02-29"
        );
    }
}
