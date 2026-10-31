//! Nautilus results → [`BacktestReport`].

use std::collections::BTreeMap;

use nautilus_backtest::result::BacktestResult;
#[cfg(test)]
use persephone_core::Timeframe;
use persephone_core::{Candle, Side, UnixNanos, signal::Note};

/// A fill as the strategy saw it, before it is matched to a bar.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawFill {
    pub(crate) ts: UnixNanos,
    pub(crate) side: Side,
    pub(crate) price: f64,
    pub(crate) qty: f64,
    pub(crate) fee: f64,
    pub(crate) maker: bool,
}

/// One execution, for trade markers on the chart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fill {
    pub ts: UnixNanos,
    /// Index of the candle whose close triggered it.
    pub bar: usize,
    pub side: Side,
    pub price: f64,
    pub qty: f64,
    /// Commission paid, in the quote currency.
    pub fee: f64,
    /// It rested on the book rather than taking what was there.
    pub maker: bool,
}

/// What a finished backtest hands back to the app.
#[derive(Clone, Debug, Default)]
pub struct BacktestReport {
    pub fills: Vec<Fill>,
    /// Account value at each candle's close (cash plus the open position at that close),
    /// aligned with the request's candles.
    pub equity: Vec<f64>,
    pub starting_balance: f64,
    /// Deepest fall from a running equity peak, as a fraction (e.g. -0.045 for -4.5%).
    pub max_drawdown: f64,
    /// Commissions paid, in the quote currency.
    pub fees: f64,
    pub total_orders: usize,
    pub total_positions: usize,
    /// How many limit orders went out, and how many of them filled.
    pub limit_orders: usize,
    pub limits_filled: usize,
    /// What funding came to, positive when it cost, and what borrowing for shorts cost.
    pub funding_paid: f64,
    pub borrow_paid: f64,
    /// Every limit order that went out, and what became of it.
    pub limit_log: Vec<LimitOrderRow>,
    /// PnL statistics in the quote currency (e.g. "PnL (total)", "Win Rate").
    pub pnl: BTreeMap<String, f64>,
    /// Return statistics (e.g. "Sharpe Ratio (252 days)", "Profit Factor").
    pub returns: BTreeMap<String, f64>,
    /// General statistics (e.g. "Long Ratio").
    pub general: BTreeMap<String, f64>,
    /// Per-period returns, as Nautilus computes them.
    pub returns_series: Vec<(UnixNanos, f64)>,
    /// What the strategy had to say about the run (a plugin's AI use, look-ahead warnings).
    pub notes: Vec<Note>,
}

impl BacktestReport {
    /// Final equity minus the starting balance, open position included.
    #[must_use]
    pub fn net_pnl(&self) -> f64 {
        self.equity
            .last()
            .map_or(0.0, |e| e - self.starting_balance)
    }

    /// [`Self::net_pnl`] as a fraction of the starting balance.
    #[must_use]
    pub fn net_return(&self) -> f64 {
        if self.starting_balance == 0.0 {
            0.0
        } else {
            self.net_pnl() / self.starting_balance
        }
    }

    /// A Nautilus statistic by name from any section, if it has a value.
    #[must_use]
    pub fn stat(&self, name: &str) -> Option<f64> {
        [&self.pnl, &self.returns, &self.general]
            .into_iter()
            .find_map(|section| section.get(name))
            .copied()
            .filter(|v| v.is_finite())
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    result: &BacktestResult,
    raw: Vec<RawFill>,
    candles: &[Candle],
    closes: &[UnixNanos],
    starting_balance: f64,
    quote: &str,
    next_open: bool,
    holding: Holding<'_>,
) -> BacktestReport {
    let fills = assign_bars(&raw, closes, next_open);
    let (equity, funding_paid, borrow_paid) =
        equity_curve(&fills, candles, closes, starting_balance, &holding);
    BacktestReport {
        max_drawdown: max_drawdown(&equity),
        // Adding to zero, as an empty sum is negative zero, which prints as a loss.
        fees: fills.iter().map(|f| f.fee).sum::<f64>() + 0.0,
        fills,
        equity,
        starting_balance,
        total_orders: result.total_orders,
        total_positions: result.total_positions,
        limit_orders: 0,
        limits_filled: 0,
        funding_paid,
        borrow_paid,
        limit_log: Vec::new(),
        pnl: result.stats_pnls.get(quote).map(sorted).unwrap_or_default(),
        returns: sorted(&result.stats_returns),
        general: sorted(&result.stats_general),
        returns_series: result
            .returns_series
            .iter()
            .map(|(ts, r)| (ts.as_u64(), *r))
            .collect(),
        notes: Vec::new(),
    }
}

/// Matches each fill to the last bar that had closed by then (fills happen on a bar's close,
/// which Nautilus stamps as the bar's time). `closes` are the bars' close times, in order.
fn assign_bars(raw: &[RawFill], closes: &[UnixNanos], next_open: bool) -> Vec<Fill> {
    raw.iter()
        .map(|f| {
            let closed = closes.partition_point(|&close| close <= f.ts);
            Fill {
                ts: f.ts,
                // At an open it belongs to the bar just opening, not the last one to close.
                bar: if next_open {
                    closed.min(closes.len().saturating_sub(1))
                } else {
                    closed.saturating_sub(1)
                },
                side: f.side,
                price: f.price,
                qty: f.qty,
                fee: f.fee,
                maker: f.maker,
            }
        })
        .collect()
}

/// What became of a limit order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitOutcome {
    Filled,
    /// Its time ran out.
    Lapsed,
    /// A newer wish took its place.
    Replaced,
    /// What it would have changed had gone, as a stop or target fired.
    Cancelled,
    /// The venue wouldn't have it.
    Refused,
    /// It was still resting when the run ended.
    Open,
}

/// A limit order as the strategy saw it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawLimit {
    pub(crate) placed: UnixNanos,
    pub(crate) side: Side,
    pub(crate) price: f64,
    pub(crate) qty: f64,
    pub(crate) outcome: LimitOutcome,
    pub(crate) ended: UnixNanos,
}

/// A limit order, and how the market treated it: what shows whether those that fill are the bad ones.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimitOrderRow {
    /// The close it went out at.
    pub placed: UnixNanos,
    pub side: Side,
    pub price: f64,
    pub qty: f64,
    pub outcome: LimitOutcome,
    /// When it filled or stopped resting.
    pub ended: UnixNanos,
    /// How many bars it rested through.
    pub bars: usize,
    /// How far past its price the market traded while it rested, as a percentage. Below zero, how far
    /// off it stayed.
    pub reached_percent: f64,
    /// How far the market moved its way, from the close it went out at, one bar and 24 bars on, as
    /// percentages: above zero is the way the order wanted to go. `None` near the end of the bars.
    pub move_1_percent: Option<f64>,
    pub move_24_percent: Option<f64>,
}

/// Works out how the market treated each limit order.
pub(crate) fn limit_rows(
    raw: &[RawLimit],
    candles: &[Candle],
    closes: &[UnixNanos],
) -> Vec<LimitOrderRow> {
    raw.iter()
        .filter_map(|order| {
            let at = closes
                .partition_point(|close| *close <= order.placed)
                .checked_sub(1)?;
            let until = closes
                .partition_point(|close| *close < order.ended)
                .min(candles.len().saturating_sub(1))
                .max(at);
            let rested = candles.get(at + 1..=until).unwrap_or(&[]);
            let reached = match order.side {
                Side::Buy => {
                    let low = rested.iter().map(|c| c.low).fold(f64::MAX, f64::min);
                    (order.price - low) / order.price
                }
                Side::Sell => {
                    let high = rested.iter().map(|c| c.high).fold(f64::MIN, f64::max);
                    (high - order.price) / order.price
                }
            };
            let sign = if order.side == Side::Buy { 1.0 } else { -1.0 };
            let from = candles.get(at)?.close;
            let moved = |bars: usize| {
                candles
                    .get(at + bars)
                    .map(|c| sign * (c.close / from - 1.0) * 100.0)
            };
            Some(LimitOrderRow {
                placed: order.placed,
                side: order.side,
                price: order.price,
                qty: order.qty,
                outcome: order.outcome,
                ended: order.ended,
                bars: rested.len(),
                reached_percent: if rested.is_empty() {
                    f64::NAN
                } else {
                    reached * 100.0
                },
                move_1_percent: moved(1),
                move_24_percent: moved(24),
            })
        })
        .collect()
}

/// What holding a position costs over time, which the venue doesn't charge in a backtest.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Holding<'a> {
    /// Rates by when they're paid.
    pub(crate) funding: &'a [(UnixNanos, f64)],
    pub(crate) borrow_percent_per_year: f64,
}

/// What a year is, for charging borrowing.
pub(crate) const NANOS_PER_YEAR: f64 = 365.0 * 86_400_000_000_000.0;

/// Cash plus the position valued at each close, and what funding and borrowing took. Fills are
/// applied at the bar they belong to. What's held into a bar pays what falls due from its open up to
/// its close, at its open, so a rate stamped on the hour falls to what was held going into that hour.
fn equity_curve(
    fills: &[Fill],
    candles: &[Candle],
    closes: &[UnixNanos],
    starting_balance: f64,
    holding: &Holding<'_>,
) -> (Vec<f64>, f64, f64) {
    let mut cash = starting_balance;
    let mut position = 0.0;
    let mut next = 0;
    let mut due = 0;
    let (mut funding_paid, mut borrow_paid) = (0.0, 0.0);
    let mut last_close = candles.first().map_or(0, |candle| candle.ts);
    let equity = candles
        .iter()
        .enumerate()
        .map(|(i, candle)| {
            let close = closes.get(i).copied().unwrap_or(candle.ts);
            while let Some(&(when, rate)) = holding.funding.get(due).filter(|f| f.0 < close) {
                if when >= last_close {
                    let paid = position * candle.open * rate;
                    cash -= paid;
                    funding_paid += paid;
                }
                due += 1;
            }
            if position < 0.0 && holding.borrow_percent_per_year > 0.0 {
                let years = close.saturating_sub(last_close) as f64 / NANOS_PER_YEAR;
                let paid =
                    -position * candle.open * holding.borrow_percent_per_year / 100.0 * years;
                cash -= paid;
                borrow_paid += paid;
            }
            last_close = close;
            while let Some(fill) = fills.get(next).filter(|f| f.bar <= i) {
                let signed = if fill.side == Side::Buy {
                    fill.qty
                } else {
                    -fill.qty
                };
                position += signed;
                cash -= signed * fill.price + fill.fee;
                next += 1;
            }
            cash + position * candle.close
        })
        .collect();
    (equity, funding_paid, borrow_paid)
}

fn max_drawdown(equity: &[f64]) -> f64 {
    let mut peak = f64::MIN;
    equity.iter().fold(0.0, |worst: f64, &e| {
        peak = peak.max(e);
        if peak > 0.0 {
            worst.min(e / peak - 1.0)
        } else {
            worst
        }
    })
}

fn sorted<'a>(stats: impl IntoIterator<Item = (&'a String, &'a f64)>) -> BTreeMap<String, f64> {
    stats.into_iter().map(|(k, v)| (k.clone(), *v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candles(closes: &[f64]) -> Vec<Candle> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &close)| Candle {
                ts: i as u64 * Timeframe::M1.nanos(),
                open: close,
                high: close,
                low: close,
                close,
                volume: 1.0,
            })
            .collect()
    }

    fn fill(bar: usize, side: Side, price: f64, qty: f64, fee: f64) -> Fill {
        Fill {
            ts: 0,
            bar,
            side,
            price,
            qty,
            fee,
            maker: false,
        }
    }

    #[test]
    fn equity_marks_the_position_to_each_close() {
        let data = candles(&[100.0, 100.0, 110.0, 120.0, 90.0]);
        // Long 2 at 100 on bar 1 (fee 1), reverse to short 2 at 120 on bar 3 (fee 2).
        let fills = [
            fill(1, Side::Buy, 100.0, 2.0, 1.0),
            fill(3, Side::Sell, 120.0, 4.0, 2.0),
        ];
        let closes: Vec<u64> = data.iter().map(|c| c.ts).collect();
        let (equity, _, _) = equity_curve(&fills, &data, &closes, 1_000.0, &Holding::default());
        assert_eq!(equity, vec![1_000.0, 999.0, 1_019.0, 1_037.0, 1_097.0]);
        assert!((max_drawdown(&equity) - (999.0 / 1_000.0 - 1.0)).abs() < 1e-12);
    }

    #[test]
    fn fills_land_on_the_bar_whose_close_triggered_them() {
        let data = candles(&[1.0, 2.0, 3.0]);
        let tf = Timeframe::M1.nanos();
        let raw = [RawFill {
            ts: 2 * tf,
            side: Side::Buy,
            price: 2.0,
            qty: 1.0,
            fee: 0.0,
            maker: false,
        }];
        let closes: Vec<u64> = data.iter().map(|c| c.ts + tf).collect();
        assert_eq!(
            assign_bars(&raw, &closes, false)[0].bar,
            1,
            "bar 1 closes at 2 minutes"
        );
    }

    #[test]
    fn drawdown_is_zero_for_a_rising_curve() {
        assert_eq!(max_drawdown(&[1.0, 2.0, 3.0]), 0.0);
        assert!((max_drawdown(&[100.0, 50.0, 200.0, 150.0]) + 0.5).abs() < 1e-12);
    }
}
