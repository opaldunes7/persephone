//! Reading timestamps the many ways old CSV files write them.

use std::{fmt, str::FromStr};

use anyhow::{Context, bail};
use persephone_core::{
    UnixNanos,
    time::{NANOS_PER_DAY, days_from_civil},
};
use serde::{Deserialize, Serialize};

/// How a file writes its timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TimeFormat {
    /// Seconds since the Unix epoch, maybe with a fraction (`1315922016.25`).
    EpochSeconds,
    EpochMillis,
    EpochMicros,
    EpochNanos,
    /// Year first: `2014-01-02 03:04:05.123`, `2014.01.02T03:04:05Z`,
    /// `20140102 03:04:05.123`, `20140102 030405123`, ...
    YearFirst,
    /// `02.01.2014 03:04:05` (day, month, year).
    DayFirst,
    /// `01/02/2014 03:04:05` (month, day, year).
    MonthFirst,
}

impl TimeFormat {
    #[must_use]
    pub const fn is_epoch(self) -> bool {
        matches!(
            self,
            Self::EpochSeconds | Self::EpochMillis | Self::EpochMicros | Self::EpochNanos
        )
    }

    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::EpochSeconds => "seconds since 1970",
            Self::EpochMillis => "milliseconds since 1970",
            Self::EpochMicros => "microseconds since 1970",
            Self::EpochNanos => "nanoseconds since 1970",
            Self::YearFirst => "date and time, year first",
            Self::DayFirst => "date and time, day first",
            Self::MonthFirst => "date and time, month first",
        }
    }

    /// Nanoseconds per unit of an epoch format.
    const fn unit(self) -> u64 {
        match self {
            Self::EpochSeconds => 1_000_000_000,
            Self::EpochMillis => 1_000_000,
            Self::EpochMicros => 1_000,
            _ => 1,
        }
    }
}

/// The time zone of timestamps written without one.
#[derive(Clone, Debug, Default)]
pub enum Zone {
    #[default]
    Utc,
    /// A fixed offset east of UTC, in seconds (`-05:00` is -18000).
    Offset(i32),
    /// An IANA zone such as `America/New_York`, daylight saving included.
    Named(String, jiff::tz::TimeZone),
}

impl Zone {
    /// Converts a wall-clock time in this zone to UTC nanoseconds.
    fn to_utc(&self, days: i64, nanos: u64) -> Option<i128> {
        let local = i128::from(days) * i128::from(NANOS_PER_DAY) + i128::from(nanos);
        match self {
            Self::Utc => Some(local),
            Self::Offset(seconds) => Some(local - i128::from(*seconds) * 1_000_000_000),
            Self::Named(_, tz) => {
                let (y, m, d) = persephone_core::time::civil_from_days(days);
                let secs = nanos / 1_000_000_000;
                let dt = jiff::civil::DateTime::new(
                    i16::try_from(y).ok()?,
                    m as i8,
                    d as i8,
                    (secs / 3_600) as i8,
                    (secs / 60 % 60) as i8,
                    (secs % 60) as i8,
                    (nanos % 1_000_000_000) as i32,
                )
                .ok()?;
                let ts = tz.to_ambiguous_timestamp(dt).compatible().ok()?;
                Some(ts.as_nanosecond())
            }
        }
    }
}

impl Zone {
    /// The wall-clock day (since 1970-01-01) and nanoseconds into it at `ts` in this zone.
    #[must_use]
    pub fn to_local(&self, ts: UnixNanos) -> (i64, u64) {
        let local = match self {
            Self::Utc => i128::from(ts),
            Self::Offset(seconds) => i128::from(ts) + i128::from(*seconds) * 1_000_000_000,
            Self::Named(_, tz) => {
                let offset = jiff::Timestamp::from_nanosecond(i128::from(ts))
                    .map_or(0, |at| tz.to_offset(at).seconds());
                i128::from(ts) + i128::from(offset) * 1_000_000_000
            }
        };
        let day = i128::from(NANOS_PER_DAY);
        (local.div_euclid(day) as i64, local.rem_euclid(day) as u64)
    }
}

impl PartialEq for Zone {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}

impl fmt::Display for Zone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Utc => f.write_str("UTC"),
            Self::Offset(seconds) => {
                let sign = if *seconds < 0 { '-' } else { '+' };
                let minutes = seconds.unsigned_abs() / 60;
                write!(f, "{sign}{:02}:{:02}", minutes / 60, minutes % 60)
            }
            Self::Named(name, _) => f.write_str(name),
        }
    }
}

impl FromStr for Zone {
    type Err = anyhow::Error;

    /// `UTC`, a fixed offset like `-05:00` / `+0530` / `+2`, or an IANA name.
    fn from_str(s: &str) -> anyhow::Result<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("utc") || s.eq_ignore_ascii_case("gmt") || s == "Z" {
            return Ok(Self::Utc);
        }
        if s.starts_with(['+', '-']) {
            return offset(s.as_bytes())
                .map(Self::Offset)
                .with_context(|| format!("`{s}` isn't an offset like -05:00"));
        }
        let tz = jiff::tz::TimeZone::get(s).with_context(|| {
            format!("unknown time zone `{s}` (try UTC, -05:00 or America/New_York)")
        })?;
        Ok(Self::Named(s.to_owned(), tz))
    }
}

impl Serialize for Zone {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Zone {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Parses one timestamp field (or a date and time joined by a space) to UTC nanoseconds.
///
/// # Errors
///
/// Returns a message saying why the text isn't a timestamp in this format.
pub fn parse(text: &str, format: TimeFormat, zone: &Zone) -> anyhow::Result<UnixNanos> {
    let ns = if format.is_epoch() {
        epoch(text.trim(), format.unit()).context("not a number")?
    } else {
        let (days, nanos, explicit) = calendar(text, format)?;
        match explicit {
            Some(seconds) => Zone::Offset(seconds).to_utc(days, nanos),
            None => zone.to_utc(days, nanos),
        }
        .context("not a valid local time")?
    };
    if ns < 0 || ns > i128::from(i64::MAX) {
        bail!("before 1970 or too far in the future");
    }
    Ok(ns as UnixNanos)
}

/// An epoch number (with optional fraction) times `unit`, without going through `f64`.
fn epoch(text: &str, unit: u64) -> Option<i128> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut ns = whole.parse::<i128>().ok()? * i128::from(unit);
    // Fractions below a nanosecond are dropped.
    let mut scale = i128::from(unit);
    for digit in fraction.bytes() {
        scale /= 10;
        if scale == 0 {
            break;
        }
        ns += i128::from(digit - b'0') * scale;
    }
    Some(ns)
}

/// The magnitude of an epoch number tells its unit, for timestamps from 1980 to 2100.
#[must_use]
pub fn epoch_unit(value: f64) -> Option<TimeFormat> {
    const FROM: f64 = 315_532_800.0; // 1980-01-01
    const TO: f64 = 4_102_444_800.0; // 2100-01-01
    [
        (1.0, TimeFormat::EpochSeconds),
        (1e3, TimeFormat::EpochMillis),
        (1e6, TimeFormat::EpochMicros),
        (1e9, TimeFormat::EpochNanos),
    ]
    .into_iter()
    .find(|(scale, _)| value >= FROM * scale && value < TO * scale)
    .map(|(_, format)| format)
}

/// Calendar text → (days since 1970, nanoseconds into the day, offset written in the text).
///
/// Works on the runs of digits, so the separators can be anything: `-`, `.`, `/`, `T`, `:`,
/// spaces. A trailing `Z`, `UTC`/`GMT` or `±HH[:MM]` sets the offset.
pub(crate) fn calendar(text: &str, format: TimeFormat) -> anyhow::Result<(i64, u64, Option<i32>)> {
    let (body, explicit) = split_offset(text.trim());
    let mut buffer = [(0, 0); MAX_RUNS];
    let runs = digit_runs(body, &mut buffer);
    if runs.is_empty() {
        bail!("no digits");
    }

    let (year, month, day, rest) = match (format, runs[0].1) {
        (TimeFormat::YearFirst, 8) => {
            let v = runs[0].0;
            (
                v / 10_000,
                (v / 100 % 100) as u32,
                (v % 100) as u32,
                &runs[1..],
            )
        }
        (TimeFormat::YearFirst, 4) if runs.len() >= 3 => {
            (runs[0].0, runs[1].0 as u32, runs[2].0 as u32, &runs[3..])
        }
        (TimeFormat::DayFirst | TimeFormat::MonthFirst, 1 | 2)
            if runs.len() >= 3 && runs[2].1 == 4 =>
        {
            let (a, b) = (runs[0].0 as u32, runs[1].0 as u32);
            let (month, day) = if format == TimeFormat::DayFirst {
                (b, a)
            } else {
                (a, b)
            };
            (runs[2].0, month, day, &runs[3..])
        }
        _ => bail!("doesn't look like a {}", format.describe()),
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !(1900..=2200).contains(&year) {
        bail!("no such date");
    }

    let nanos = time_of_day(rest).context("not a time of day")?;
    Ok((days_from_civil(year, month, day), nanos, explicit))
}

/// `HH:MM[:SS[.fff]]` as runs, or packed `HHMM`, `HHMMSS`, `HHMMSSfff...`.
fn time_of_day(runs: &[(i64, usize)]) -> Option<u64> {
    let (h, m, s, fraction) = match runs {
        [] => (0, 0, 0, None),
        [(v, 4)] => (v / 100, v % 100, 0, None),
        [(v, 6)] => (v / 10_000, v / 100 % 100, v % 100, None),
        [(v, len @ 7..=15)] => {
            let digits = (*len - 6) as u32;
            let packed = 10i64.pow(digits);
            let hms = v / packed;
            (
                hms / 10_000,
                hms / 100 % 100,
                hms % 100,
                Some((v % packed, *len - 6)),
            )
        }
        [(h, _), (m, _)] => (*h, *m, 0, None),
        [(h, _), (m, _), (s, _)] => (*h, *m, *s, None),
        [(h, _), (m, _), (s, _), (f, len)] => (*h, *m, *s, Some((*f, *len))),
        _ => return None,
    };
    if h > 24 || m > 59 || s > 60 {
        return None;
    }
    let mut nanos = ((h * 60 + m) * 60 + s) as u64 * 1_000_000_000;
    if let Some((value, digits)) = fraction {
        // Keep up to nine digits of the fraction.
        let value = if digits > 9 {
            value / 10i64.pow((digits - 9) as u32)
        } else {
            value * 10i64.pow((9 - digits) as u32)
        };
        nanos += value as u64;
    }
    Some(nanos)
}

const MAX_RUNS: usize = 10;

/// The runs of digits in `text` as (value, length), up to [`MAX_RUNS`], written into `buffer`
/// (no allocation: this runs for every row). A run longer than 18 digits ends the scan.
fn digit_runs<'a>(text: &str, buffer: &'a mut [(i64, usize); MAX_RUNS]) -> &'a [(i64, usize)] {
    let mut count = 0;
    let (mut value, mut len) = (0i64, 0usize);
    for b in text.bytes() {
        if b.is_ascii_digit() {
            if len == 18 {
                break;
            }
            value = value * 10 + i64::from(b - b'0');
            len += 1;
        } else if len > 0 {
            if count == MAX_RUNS {
                break;
            }
            buffer[count] = (value, len);
            count += 1;
            (value, len) = (0, 0);
        }
    }
    if len > 0 && count < MAX_RUNS {
        buffer[count] = (value, len);
        count += 1;
    }
    &buffer[..count]
}

/// Splits a trailing `Z`, `UTC`, `GMT`, `GMT+0200` or `±HH[:MM]` off a timestamp.
fn split_offset(text: &str) -> (&str, Option<i32>) {
    let mut body = text.trim_end();
    if let Some(stripped) = body.strip_suffix('Z') {
        return (stripped, Some(0));
    }
    for name in ["UTC", "GMT"] {
        if let Some(stripped) = body.strip_suffix(name) {
            return (stripped.trim_end(), Some(0));
        }
    }
    // A sign after the time of day starts an offset; a date's `-` comes before any `:`.
    let Some(colon) = body.find(':') else {
        return (body, None);
    };
    if let Some(sign) = body.rfind(['+', '-']).filter(|&i| i > colon)
        && let Some(seconds) = offset(&body.as_bytes()[sign..])
    {
        body = body[..sign].trim_end();
        let body = body
            .strip_suffix("UTC")
            .or_else(|| body.strip_suffix("GMT"))
            .unwrap_or(body);
        return (body.trim_end(), Some(seconds));
    }
    (body, None)
}

/// `+HH`, `+HHMM` or `+HH:MM` (or `-`) → seconds east of UTC.
fn offset(text: &[u8]) -> Option<i32> {
    let sign = match text.first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: Vec<i32> = text[1..]
        .iter()
        .filter(|&&b| b != b':')
        .map(|&b| b.is_ascii_digit().then(|| i32::from(b - b'0')))
        .collect::<Option<_>>()?;
    let (h, m) = match digits.as_slice() {
        [h] => (*h, 0),
        [h1, h2] => (h1 * 10 + h2, 0),
        [h1, h2, m1, m2] => (h1 * 10 + h2, m1 * 10 + m2),
        _ => return None,
    };
    (h <= 14 && m < 60).then_some(sign * (h * 3_600 + m * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_times_read_on_local_clocks() {
        // 2024-07-01 14:30 UTC is 10:30 in New York (daylight saving) and 15:30 at +01:00.
        let ts = parse("2024-07-01 14:30:00", TimeFormat::YearFirst, &Zone::Utc).unwrap();
        let day = persephone_core::time::days_from_civil(2024, 7, 1);
        let minutes = |(d, nanos): (i64, u64)| (d - day, nanos / 60_000_000_000);
        let new_york: Zone = "America/New_York".parse().unwrap();
        assert_eq!(minutes(new_york.to_local(ts)), (0, 10 * 60 + 30));
        assert_eq!(
            minutes("+01:00".parse::<Zone>().unwrap().to_local(ts)),
            (0, 15 * 60 + 30)
        );
        assert_eq!(
            minutes("-14:30".parse::<Zone>().unwrap().to_local(ts)),
            (0, 0),
            "midnight"
        );
        assert_eq!(
            minutes("-14:45".parse::<Zone>().unwrap().to_local(ts)),
            (-1, 23 * 60 + 45)
        );
        assert_eq!(minutes(Zone::Utc.to_local(ts)), (0, 14 * 60 + 30));
        // In January New York is on standard time.
        let winter = parse("2024-01-15 14:30:00", TimeFormat::YearFirst, &Zone::Utc).unwrap();
        assert_eq!(new_york.to_local(winter).1 / 60_000_000_000, 9 * 60 + 30);
    }

    /// 2014-01-02 03:04:05 UTC.
    const T: UnixNanos = 1_388_631_845_000_000_000;

    fn utc(text: &str, format: TimeFormat) -> UnixNanos {
        parse(text, format, &Zone::Utc).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn reads_year_first_text_in_its_many_spellings() {
        use TimeFormat::YearFirst as Y;
        for text in [
            "2014-01-02 03:04:05",
            "2014-01-02T03:04:05Z",
            "2014.01.02 03:04:05",
            "2014/01/02 03:04:05",
            "20140102 03:04:05",
            "20140102 030405",
            "20140102T030405",
            "2014-01-02 03:04:05 UTC",
            "2014-01-02 05:04:05+02:00",
            "2014-01-02 00:04:05-0300",
            "2014.01.02 05:04:05 GMT+0200",
        ] {
            assert_eq!(utc(text, Y), T, "{text}");
        }
        assert_eq!(utc("2014-01-02 03:04:05.123", Y), T + 123_000_000);
        assert_eq!(utc("2014-01-02 03:04:05,5", Y), T + 500_000_000);
        assert_eq!(utc("20140102 030405123", Y), T + 123_000_000, "HistData");
        assert_eq!(utc("2014-01-02T03:04:05.123456789Z", Y), T + 123_456_789);
        assert_eq!(utc("2014-01-02T03:04:05.1234567891Z", Y), T + 123_456_789);
        assert_eq!(utc("2014-01-02 03:04", Y), T - 5_000_000_000);
        assert_eq!(
            utc("2014-01-02", Y),
            T - (3 * 3_600 + 4 * 60 + 5) * 1_000_000_000
        );
    }

    #[test]
    fn reads_day_and_month_first_dates() {
        assert_eq!(
            utc("02.01.2014 03:04:05.000", TimeFormat::DayFirst),
            T,
            "Dukascopy"
        );
        assert_eq!(utc("01/02/2014 03:04:05", TimeFormat::MonthFirst), T);
        assert!(parse("13/02/2014 03:04:05", TimeFormat::MonthFirst, &Zone::Utc).is_err());
    }

    #[test]
    fn reads_epoch_numbers_exactly() {
        assert_eq!(utc("1388631845", TimeFormat::EpochSeconds), T);
        assert_eq!(
            utc("1388631845.25", TimeFormat::EpochSeconds),
            T + 250_000_000
        );
        assert_eq!(
            utc("1388631845123", TimeFormat::EpochMillis),
            T + 123_000_000
        );
        assert_eq!(
            utc("1388631845123456", TimeFormat::EpochMicros),
            T + 123_456_000
        );
        assert_eq!(
            utc("1388631845123456789", TimeFormat::EpochNanos),
            T + 123_456_789
        );
        assert!(parse("12ab", TimeFormat::EpochSeconds, &Zone::Utc).is_err());

        assert_eq!(epoch_unit(1.388e9), Some(TimeFormat::EpochSeconds));
        assert_eq!(epoch_unit(1.388e12), Some(TimeFormat::EpochMillis));
        assert_eq!(epoch_unit(1.388e15), Some(TimeFormat::EpochMicros));
        assert_eq!(epoch_unit(1.388e18), Some(TimeFormat::EpochNanos));
        assert_eq!(epoch_unit(12_345.0), None, "a trade id, not a time");
    }

    #[test]
    fn applies_zones_including_daylight_saving() {
        let est: Zone = "-05:00".parse().unwrap();
        assert_eq!(est.to_string(), "-05:00");
        let text = "2014-01-01 22:04:05";
        assert_eq!(parse(text, TimeFormat::YearFirst, &est).unwrap(), T);

        let new_york: Zone = "America/New_York".parse().unwrap();
        assert_eq!(
            parse(text, TimeFormat::YearFirst, &new_york).unwrap(),
            T,
            "winter: -5"
        );
        let summer = parse("2014-07-01 12:00:00", TimeFormat::YearFirst, &new_york).unwrap();
        let summer_utc = parse("2014-07-01 16:00:00", TimeFormat::YearFirst, &Zone::Utc).unwrap();
        assert_eq!(summer, summer_utc, "summer: -4");

        // A written offset wins over the zone.
        assert_eq!(
            parse("2014-01-02 03:04:05Z", TimeFormat::YearFirst, &est).unwrap(),
            T
        );
        assert!("Mars/Olympus".parse::<Zone>().is_err());
        assert_eq!("+0530".parse::<Zone>().unwrap(), Zone::Offset(19_800));
    }
}
