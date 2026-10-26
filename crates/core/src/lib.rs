//! Types shared by every Persephone crate.
//!
//! Nothing here depends on Nautilus: the chart, the data layer and the plugin host all speak
//! these types, and only `persephone-engine` translates them into Nautilus types.

pub mod indicator;
pub mod signal;
pub mod strategy;
pub mod time;

use std::{fmt, str::FromStr};

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

/// Bar duration. The set matches what crypto venues serve natively.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Timeframe {
    M1,
    M5,
    M15,
    M30,
    H1,
    H4,
    D1,
}

impl Timeframe {
    pub const ALL: [Self; 7] = [
        Self::M1,
        Self::M5,
        Self::M15,
        Self::M30,
        Self::H1,
        Self::H4,
        Self::D1,
    ];

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
        }
    }

    /// Short label, also the interval string Binance uses ("1m", "4h", ...).
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
        }
    }
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown timeframe `{0}` (expected one of 1m, 5m, 15m, 30m, 1h, 4h, 1d)")]
pub struct ParseTimeframeError(String);

impl FromStr for Timeframe {
    type Err = ParseTimeframeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|tf| tf.label().eq_ignore_ascii_case(s))
            .ok_or_else(|| ParseTimeframeError(s.to_owned()))
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
        assert!("2h".parse::<Timeframe>().is_err());
    }

    #[test]
    fn timeframe_nanos_are_ordered() {
        let nanos: Vec<_> = Timeframe::ALL.iter().map(|tf| tf.nanos()).collect();
        assert!(nanos.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(Timeframe::H1.nanos(), 3_600_000_000_000);
    }
}
