//! Types shared by every Persephone crate.
//!
//! Nothing here depends on Nautilus: the chart, the data layer and the plugin host all speak
//! these types, and only `persephone-engine` translates them into Nautilus types.

pub mod indicator;
pub mod signal;
pub mod strategy;
pub mod tick;
pub mod time;

use std::{fmt, str::FromStr};

use crate::time::{NANOS_PER_DAY, civil_from_days, days_from_civil};

/// Nanoseconds since the Unix epoch, UTC.
pub type UnixNanos = u64;

/// One OHLCV bar. `ts` is the bar's **open** time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candle {
    pub ts: UnixNanos,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// Bar duration. The set matches what crypto venues serve natively. Weeks start on Monday
/// and months on the 1st, both at midnight UTC, as Binance's do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Timeframe {
    M1,
    M5,
    M15,
    M30,
    H1,
    H4,
    D1,
    W1,
    MN1,
}

impl Timeframe {
    pub const ALL: [Self; 9] = [
        Self::M1,
        Self::M5,
        Self::M15,
        Self::M30,
        Self::H1,
        Self::H4,
        Self::D1,
        Self::W1,
        Self::MN1,
    ];

    /// The bar's length; a month counts as 30 days here, so use [`open_time`](Self::open_time)
    /// and [`close_time`](Self::close_time) for a bar's edges.
    #[must_use]
    pub const fn nanos(self) -> UnixNanos {
        const MINUTE: UnixNanos = 60_000_000_000;
        match self {
            Self::M1 => MINUTE,
            Self::M5 => 5 * MINUTE,
            Self::M15 => 15 * MINUTE,
            Self::M30 => 30 * MINUTE,
            Self::H1 => 60 * MINUTE,
            Self::H4 => 240 * MINUTE,
            Self::D1 => 1_440 * MINUTE,
            Self::W1 => 7 * 1_440 * MINUTE,
            Self::MN1 => 30 * 1_440 * MINUTE,
        }
    }

    /// Short label, also the interval string Binance uses ("1m", "4h", "1M", ...).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::M1 => "1m",
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::M30 => "30m",
            Self::H1 => "1h",
            Self::H4 => "4h",
            Self::D1 => "1d",
            Self::W1 => "1w",
            Self::MN1 => "1M",
        }
    }

    /// When the bar holding `ts` opens.
    #[must_use]
    pub fn open_time(self, ts: UnixNanos) -> UnixNanos {
        let day = (ts / NANOS_PER_DAY) as i64;
        let start = |day: i64| day.max(0) as UnixNanos * NANOS_PER_DAY;
        match self {
            // 1970-01-05 was the first Monday.
            Self::W1 => start(day - (day - 4).rem_euclid(7)),
            Self::MN1 => {
                let (year, month, _) = civil_from_days(day);
                start(days_from_civil(year, month, 1))
            }
            _ => ts - ts % self.nanos(),
        }
    }

    /// When the bar holding `ts` closes, which is when the next one opens.
    #[must_use]
    pub fn close_time(self, ts: UnixNanos) -> UnixNanos {
        let open = self.open_time(ts);
        match self {
            Self::MN1 => {
                let (year, month, _) = civil_from_days((open / NANOS_PER_DAY) as i64);
                let (year, month) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
                days_from_civil(year, month, 1) as UnixNanos * NANOS_PER_DAY
            }
            _ => open + self.nanos(),
        }
    }

    /// Whether this timeframe's bars are made of whole bars of `shorter`: every timeframe up
    /// to a day divides a day, and days make weeks and months, but weeks don't make months.
    #[must_use]
    pub fn is_made_of(self, shorter: Self) -> bool {
        match (shorter, self) {
            (Self::W1, other) => other == Self::W1,
            (Self::MN1, other) => other == Self::MN1,
            (_, Self::W1 | Self::MN1) => true,
            (shorter, longer) => longer.nanos() % shorter.nanos() == 0,
        }
    }
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown timeframe `{0}` (expected one of 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w, 1M)")]
pub struct ParseTimeframeError(String);

impl FromStr for Timeframe {
    type Err = ParseTimeframeError;

    /// Case doesn't matter, except that `1m` is a minute and `1M` a month.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let find =
            |same: fn(&str, &str) -> bool| Self::ALL.into_iter().find(|tf| same(tf.label(), s));
        find(|a, b| a == b)
            .or_else(|| find(str::eq_ignore_ascii_case))
            .ok_or_else(|| ParseTimeframeError(s.to_owned()))
    }
}

/// How bars are cut: by time, or (from ticks) every so many ticks or every so much traded
/// size. Tick and volume bars need a market with ticks (the local store).
#[derive(Clone, Copy, Debug)]
pub enum BarSpec {
    Time(Timeframe),
    /// A bar every `n` ticks.
    Ticks(u32),
    /// A bar each time this much size has traded (trades only; quotes have no size).
    Volume(f64),
}

impl BarSpec {
    /// The timeframe of time bars.
    #[must_use]
    pub const fn timeframe(self) -> Option<Timeframe> {
        match self {
            Self::Time(tf) => Some(tf),
            Self::Ticks(_) | Self::Volume(_) => None,
        }
    }

    /// Short form, also how it's saved and typed: `15m`, `1000t`, `2.5v`.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Time(tf) => tf.label().to_owned(),
            Self::Ticks(n) => format!("{n}t"),
            Self::Volume(v) => format!("{v}v"),
        }
    }

    /// When each bar closes. Time bars close a timeframe after they open. Tick and volume bars
    /// close just before the next one opens, so a strategy sees a bar only after its last tick;
    /// the last bar closes a typical bar length after it opens.
    #[must_use]
    pub fn closes(self, candles: &[Candle]) -> Vec<UnixNanos> {
        match self {
            Self::Time(tf) => candles.iter().map(|c| tf.close_time(c.ts)).collect(),
            Self::Ticks(_) | Self::Volume(_) => {
                let typical = self.typical_nanos(candles);
                let mut closes: Vec<UnixNanos> = candles
                    .windows(2)
                    .map(|w| w[1].ts.saturating_sub(1).max(w[0].ts))
                    .collect();
                if let Some(last) = candles.last() {
                    closes.push(last.ts + typical);
                }
                closes
            }
        }
    }

    /// A bar's usual length: the timeframe, or the median gap between bar opens.
    #[must_use]
    pub fn typical_nanos(self, candles: &[Candle]) -> UnixNanos {
        if let Self::Time(tf) = self {
            return tf.nanos();
        }
        let mut gaps: Vec<UnixNanos> = candles.windows(2).map(|w| w[1].ts - w[0].ts).collect();
        if gaps.is_empty() {
            return Timeframe::M1.nanos();
        }
        let middle = gaps.len() / 2;
        *gaps.select_nth_unstable(middle).1.max(&mut 1)
    }
}

impl From<Timeframe> for BarSpec {
    fn from(tf: Timeframe) -> Self {
        Self::Time(tf)
    }
}

impl PartialEq for BarSpec {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Time(a), Self::Time(b)) => a == b,
            (Self::Ticks(a), Self::Ticks(b)) => a == b,
            (Self::Volume(a), Self::Volume(b)) => a.to_bits() == b.to_bits(),
            _ => false,
        }
    }
}

impl Eq for BarSpec {}

impl std::hash::Hash for BarSpec {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Time(tf) => tf.hash(state),
            Self::Ticks(n) => n.hash(state),
            Self::Volume(v) => v.to_bits().hash(state),
        }
    }
}

impl fmt::Display for BarSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Time(tf) => write!(f, "{tf}"),
            Self::Ticks(n) => write!(f, "{n} ticks"),
            Self::Volume(v) => write!(f, "{v} volume"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "unknown bars `{0}` (a timeframe such as 1m or 4h, ticks such as 1000t, or volume such as 2.5v)"
)]
pub struct ParseBarSpecError(String);

impl FromStr for BarSpec {
    type Err = ParseBarSpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Ok(tf) = s.parse::<Timeframe>() {
            return Ok(Self::Time(tf));
        }
        let error = || ParseBarSpecError(s.to_owned());
        if let Some(n) = s.strip_suffix(['t', 'T']) {
            let n: u32 = n.trim().parse().map_err(|_| error())?;
            return if n >= 2 {
                Ok(Self::Ticks(n))
            } else {
                Err(error())
            };
        }
        if let Some(v) = s.strip_suffix(['v', 'V']) {
            let v: f64 = v.trim().parse().map_err(|_| error())?;
            return if v.is_finite() && v > 0.0 {
                Ok(Self::Volume(v))
            } else {
                Err(error())
            };
        }
        Err(error())
    }
}

/// What the engine needs to know about a spot instrument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstrumentSpec {
    /// Venue symbol, e.g. `BTCUSDT`.
    pub symbol: String,
    /// Venue name, e.g. `BINANCE`.
    pub venue: String,
    pub base: String,
    pub quote: String,
    /// Decimal places of the price tick (0.01 → 2).
    pub price_precision: u8,
    /// Decimal places of the size step (0.00001 → 5).
    pub size_precision: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeframe_labels_round_trip() {
        for tf in Timeframe::ALL {
            assert_eq!(tf.label().parse::<Timeframe>().unwrap(), tf);
        }
        assert_eq!("4H".parse::<Timeframe>().unwrap(), Timeframe::H4);
        assert_eq!("1W".parse::<Timeframe>().unwrap(), Timeframe::W1);
        assert_eq!("1m".parse::<Timeframe>().unwrap(), Timeframe::M1);
        assert_eq!("1M".parse::<Timeframe>().unwrap(), Timeframe::MN1);
        assert!("2h".parse::<Timeframe>().is_err());
    }

    #[test]
    fn weeks_start_on_monday_and_months_on_the_first() {
        let at = |y, m, d, hour: u64| {
            time::days_from_civil(y, m, d) as UnixNanos * NANOS_PER_DAY + hour * 3_600_000_000_000
        };
        // 2024-02-29 was a Thursday in a leap year.
        let ts = at(2024, 2, 29, 13);
        assert_eq!(Timeframe::W1.open_time(ts), at(2024, 2, 26, 0));
        assert_eq!(Timeframe::W1.close_time(ts), at(2024, 3, 4, 0));
        assert_eq!(
            Timeframe::W1.open_time(at(2024, 3, 4, 0)),
            at(2024, 3, 4, 0)
        );
        assert_eq!(Timeframe::MN1.open_time(ts), at(2024, 2, 1, 0));
        assert_eq!(Timeframe::MN1.close_time(ts), at(2024, 3, 1, 0));
        assert_eq!(
            Timeframe::MN1.close_time(at(2023, 12, 31, 23)),
            at(2024, 1, 1, 0)
        );
        // A week can straddle a year.
        assert_eq!(
            Timeframe::W1.open_time(at(2025, 1, 1, 0)),
            at(2024, 12, 30, 0)
        );
        assert_eq!(Timeframe::H4.open_time(ts), at(2024, 2, 29, 12));
        assert_eq!(Timeframe::H4.close_time(ts), at(2024, 2, 29, 16));
        assert_eq!(Timeframe::W1.open_time(0), 0, "before the first Monday");
    }

    #[test]
    fn longer_timeframes_are_made_of_shorter_ones() {
        assert!(Timeframe::H4.is_made_of(Timeframe::M15));
        assert!(Timeframe::W1.is_made_of(Timeframe::D1));
        assert!(Timeframe::MN1.is_made_of(Timeframe::H4));
        assert!(!Timeframe::MN1.is_made_of(Timeframe::W1));
        assert!(Timeframe::W1.is_made_of(Timeframe::W1));
    }

    #[test]
    fn bar_specs_parse_and_print() {
        for text in ["15m", "1000t", "2.5v", "4h"] {
            assert_eq!(text.parse::<BarSpec>().unwrap().label(), text);
        }
        assert_eq!("1000T".parse::<BarSpec>().unwrap(), BarSpec::Ticks(1000));
        assert_eq!(BarSpec::Ticks(500).to_string(), "500 ticks");
        assert!(
            "1t".parse::<BarSpec>().is_err(),
            "a bar per tick is just ticks"
        );
        assert!("0v".parse::<BarSpec>().is_err());
        assert!("2h".parse::<BarSpec>().is_err());
    }

    #[test]
    fn irregular_bars_close_just_before_the_next_opens() {
        let candle = |ts| Candle {
            ts,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        };
        let candles = [candle(100), candle(130), candle(200), candle(240)];
        assert_eq!(
            BarSpec::Ticks(10).closes(&candles),
            vec![129, 199, 239, 280]
        );
        assert_eq!(
            BarSpec::Ticks(10).typical_nanos(&candles),
            40,
            "the median gap"
        );
        let minute = Timeframe::M1.nanos();
        assert_eq!(
            BarSpec::Time(Timeframe::M1).closes(&[candle(0)]),
            vec![minute]
        );
    }

    #[test]
    fn timeframe_nanos_are_ordered() {
        let nanos: Vec<_> = Timeframe::ALL.iter().map(|tf| tf.nanos()).collect();
        assert!(nanos.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(Timeframe::H1.nanos(), 3_600_000_000_000);
    }
}
