//! Ticks: single trades and top-of-book quotes, the raw data behind bars.
//!
//! A market holds one kind of tick. Charts see them as bars ([`bars`]); tick-level backtests
//! replay them through a [`TickFeed`] so orders fill at tick prices.

use std::fmt;

use crate::{Candle, Timeframe, UnixNanos};

/// Which side took liquidity in a trade.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Aggressor {
    Buyer,
    Seller,
    #[default]
    Unknown,
}

/// One trade. A `size` of 0 means the source didn't record one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trade {
    pub ts: UnixNanos,
    pub price: f64,
    pub size: f64,
    pub aggressor: Aggressor,
}

/// The best bid and ask at one moment. Sizes of 0 mean the source didn't record them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quote {
    pub ts: UnixNanos,
    pub bid: f64,
    pub ask: f64,
    pub bid_size: f64,
    pub ask_size: f64,
}

impl Quote {
    #[must_use]
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TickKind {
    Trades,
    Quotes,
}

impl TickKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Trades => "trades",
            Self::Quotes => "quotes",
        }
    }
}

impl fmt::Display for TickKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A run of ticks of one kind, oldest first.
#[derive(Clone, Debug, PartialEq)]
pub enum Ticks {
    Trades(Vec<Trade>),
    Quotes(Vec<Quote>),
}

impl Ticks {
    #[must_use]
    pub const fn empty(kind: TickKind) -> Self {
        match kind {
            TickKind::Trades => Self::Trades(Vec::new()),
            TickKind::Quotes => Self::Quotes(Vec::new()),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> TickKind {
        match self {
            Self::Trades(_) => TickKind::Trades,
            Self::Quotes(_) => TickKind::Quotes,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Trades(t) => t.len(),
            Self::Quotes(q) => q.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Timestamp of tick `i`.
    #[must_use]
    pub fn ts(&self, i: usize) -> UnixNanos {
        match self {
            Self::Trades(t) => t[i].ts,
            Self::Quotes(q) => q[i].ts,
        }
    }
}

/// Where a tick-level backtest reads its ticks.
pub trait TickFeed: Send + Sync + fmt::Debug {
    /// Every tick with `start <= ts < end`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a message if the ticks can't be read.
    fn ticks(&self, start: UnixNanos, end: UnixNanos) -> Result<Ticks, String>;
}

/// Aggregates ticks (oldest first) into bars of `timeframe`, keyed by open time.
///
/// Trades make bars from their prices; volume is the traded size, or the number of trades when
/// the source has no sizes. Quotes make bars from the mid price, with the number of quotes as
/// volume ("tick volume"). Periods without ticks have no bar.
#[must_use]
pub fn bars(ticks: &Ticks, timeframe: Timeframe) -> Vec<Candle> {
    let mut builder = BarBuilder::new(timeframe);
    match ticks {
        Ticks::Trades(trades) => {
            for t in trades {
                let volume = if t.size > 0.0 { t.size } else { 1.0 };
                builder.push(t.ts, t.price, volume);
            }
        }
        Ticks::Quotes(quotes) => {
            for q in quotes {
                builder.push(q.ts, q.mid(), 1.0);
            }
        }
    }
    builder.finish()
}

/// Aggregates ticks (oldest first) into bars of `spec`: time bars as [`bars`] does, or a bar
/// every so many ticks, or each time so much size has traded. Volume bars from quotes (which
/// have no size) come out empty.
#[must_use]
pub fn bars_by(ticks: &Ticks, spec: crate::BarSpec) -> Vec<Candle> {
    use crate::BarSpec;
    match spec {
        BarSpec::Time(timeframe) => bars(ticks, timeframe),
        BarSpec::Ticks(n) => {
            let n = n.max(1) as usize;
            let mut out = Vec::new();
            let mut count = 0;
            let mut push = |ts: UnixNanos, price: f64, volume: f64| {
                extend(&mut out, count == 0, ts, price, volume);
                count = (count + 1) % n;
            };
            match ticks {
                Ticks::Trades(trades) => {
                    for t in trades {
                        push(t.ts, t.price, if t.size > 0.0 { t.size } else { 1.0 });
                    }
                }
                Ticks::Quotes(quotes) => {
                    for q in quotes {
                        push(q.ts, q.mid(), 1.0);
                    }
                }
            }
            out
        }
        BarSpec::Volume(threshold) => {
            let Ticks::Trades(trades) = ticks else {
                return Vec::new();
            };
            let mut out = Vec::new();
            let mut filled = threshold; // start a bar on the first trade
            for t in trades {
                let size = if t.size > 0.0 { t.size } else { 1.0 };
                let new = filled >= threshold;
                if new {
                    filled = 0.0;
                }
                extend(&mut out, new, t.ts, t.price, size);
                filled += size;
            }
            out
        }
    }
}

/// Starts a bar at this tick, or adds the tick to the last one.
fn extend(bars: &mut Vec<Candle>, new: bool, ts: UnixNanos, price: f64, volume: f64) {
    match bars.last_mut() {
        Some(bar) if !new => {
            bar.high = bar.high.max(price);
            bar.low = bar.low.min(price);
            bar.close = price;
            bar.volume += volume;
        }
        _ => bars.push(Candle {
            ts,
            open: price,
            high: price,
            low: price,
            close: price,
            volume,
        }),
    }
}

/// Rolls bars (oldest first) up into a longer `timeframe`. Every [`Timeframe`] divides a day,
/// so bars of the shorter timeframe never straddle a longer one.
#[must_use]
pub fn resample(bars: &[Candle], timeframe: Timeframe) -> Vec<Candle> {
    let step = timeframe.nanos();
    let mut out: Vec<Candle> = Vec::new();
    for bar in bars {
        let open_time = bar.ts - bar.ts % step;
        match out.last_mut() {
            Some(last) if last.ts == open_time => {
                last.high = last.high.max(bar.high);
                last.low = last.low.min(bar.low);
                last.close = bar.close;
                last.volume += bar.volume;
            }
            _ => out.push(Candle {
                ts: open_time,
                ..*bar
            }),
        }
    }
    out
}

struct BarBuilder {
    step: UnixNanos,
    bars: Vec<Candle>,
}

impl BarBuilder {
    fn new(timeframe: Timeframe) -> Self {
        Self {
            step: timeframe.nanos(),
            bars: Vec::new(),
        }
    }

    fn push(&mut self, ts: UnixNanos, price: f64, volume: f64) {
        let open_time = ts - ts % self.step;
        match self.bars.last_mut() {
            Some(bar) if bar.ts == open_time => {
                bar.high = bar.high.max(price);
                bar.low = bar.low.min(price);
                bar.close = price;
                bar.volume += volume;
            }
            _ => self.bars.push(Candle {
                ts: open_time,
                open: price,
                high: price,
                low: price,
                close: price,
                volume,
            }),
        }
    }

    fn finish(self) -> Vec<Candle> {
        self.bars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: UnixNanos = 60_000_000_000;

    fn trade(ts: UnixNanos, price: f64, size: f64) -> Trade {
        Trade {
            ts,
            price,
            size,
            aggressor: Aggressor::Unknown,
        }
    }

    #[test]
    fn trades_make_bars_by_open_time() {
        let ticks = Ticks::Trades(vec![
            trade(10, 100.0, 1.0),
            trade(MIN - 1, 103.0, 2.0),
            trade(MIN + 5, 99.0, 0.5),
            trade(3 * MIN, 101.0, 1.0),
        ]);
        let bars = bars(&ticks, Timeframe::M1);
        assert_eq!(bars.len(), 3, "the empty minute has no bar");
        assert_eq!(
            bars[0],
            Candle {
                ts: 0,
                open: 100.0,
                high: 103.0,
                low: 100.0,
                close: 103.0,
                volume: 3.0
            }
        );
        assert_eq!((bars[1].ts, bars[1].close), (MIN, 99.0));
        assert_eq!(bars[2].ts, 3 * MIN);
    }

    #[test]
    fn sizeless_trades_count_and_quotes_use_the_mid() {
        let bars_from = |ticks| bars(&ticks, Timeframe::M1)[0];
        let counted = bars_from(Ticks::Trades(vec![trade(0, 1.0, 0.0), trade(1, 2.0, 0.0)]));
        assert_eq!(counted.volume, 2.0);

        let quote = |ts, bid, ask| Quote {
            ts,
            bid,
            ask,
            bid_size: 0.0,
            ask_size: 0.0,
        };
        let mid = bars_from(Ticks::Quotes(vec![quote(0, 1.0, 1.2), quote(1, 1.3, 1.5)]));
        assert!((mid.open - 1.1).abs() < 1e-12 && (mid.close - 1.4).abs() < 1e-12);
        assert_eq!(mid.volume, 2.0);
    }

    #[test]
    fn tick_and_volume_bars() {
        let trades = Ticks::Trades(
            [(1.0, 1.0), (3.0, 2.0), (2.0, 0.5), (5.0, 0.5), (4.0, 3.0)]
                .iter()
                .enumerate()
                .map(|(i, &(price, size))| trade(i as UnixNanos, price, size))
                .collect(),
        );
        let by_two = bars_by(&trades, crate::BarSpec::Ticks(2));
        assert_eq!(by_two.len(), 3);
        assert_eq!(
            (by_two[0].open, by_two[0].high, by_two[0].close),
            (1.0, 3.0, 3.0)
        );
        assert_eq!(by_two[2].ts, 4, "the last bar is the leftover tick");

        // A bar closes once 2.5 has traded.
        let by_volume = bars_by(&trades, crate::BarSpec::Volume(2.5));
        let volumes: Vec<f64> = by_volume.iter().map(|b| b.volume).collect();
        assert_eq!(volumes, vec![3.0, 4.0]);
        assert!(bars_by(&Ticks::Quotes(Vec::new()), crate::BarSpec::Volume(1.0)).is_empty());
    }

    #[test]
    fn resampling_matches_aggregating_ticks_directly() {
        let trades: Vec<Trade> = (0..5_000)
            .map(|i| trade(i * 7 * 1_000_000_000, 100.0 + (i as f64 / 50.0).sin(), 1.0))
            .collect();
        let ticks = Ticks::Trades(trades);
        let minute = bars(&ticks, Timeframe::M1);
        for tf in [Timeframe::M5, Timeframe::M15, Timeframe::H1, Timeframe::D1] {
            assert_eq!(resample(&minute, tf), bars(&ticks, tf), "{tf}");
        }
    }
}
