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

pub(crate) fn build(
    result: &BacktestResult,
    raw: Vec<RawFill>,
    candles: &[Candle],
    closes: &[UnixNanos],
    starting_balance: f64,
    quote: &str,
) -> BacktestReport {
    let fills = assign_bars(&raw, closes);
    let equity = equity_curve(&fills, candles, starting_balance);
    BacktestReport {
        max_drawdown: max_drawdown(&equity),
        fees: fills.iter().map(|f| f.fee).sum(),
        fills,
        equity,
        starting_balance,
        total_orders: result.total_orders,
        total_positions: result.total_positions,
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
fn assign_bars(raw: &[RawFill], closes: &[UnixNanos]) -> Vec<Fill> {
    raw.iter()
        .map(|f| {
            let closed = closes.partition_point(|&close| close <= f.ts);
            Fill {
                ts: f.ts,
                bar: closed.saturating_sub(1),
                side: f.side,
                price: f.price,
                qty: f.qty,
                fee: f.fee,
            }
        })
        .collect()
}

/// Cash plus the position valued at each close. Fills are applied at the bar they belong to.
fn equity_curve(fills: &[Fill], candles: &[Candle], starting_balance: f64) -> Vec<f64> {
    let mut cash = starting_balance;
    let mut position = 0.0;
    let mut next = 0;
    candles
        .iter()
        .enumerate()
        .map(|(i, candle)| {
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
        .collect()
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
        let equity = equity_curve(&fills, &data, 1_000.0);
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
        }];
        let closes: Vec<u64> = data.iter().map(|c| c.ts + tf).collect();
        assert_eq!(
            assign_bars(&raw, &closes)[0].bar,
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
