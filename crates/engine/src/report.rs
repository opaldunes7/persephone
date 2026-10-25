//! Nautilus results → [`BacktestReport`].

use std::collections::BTreeMap;

use nautilus_backtest::result::BacktestResult;
use persephone_core::{Side, UnixNanos};

/// One execution, for trade markers on the chart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fill {
    pub ts: UnixNanos,
    pub side: Side,
    pub price: f64,
    pub qty: f64,
}

/// What a finished backtest hands back to the app.
#[derive(Clone, Debug, Default)]
pub struct BacktestReport {
    pub fills: Vec<Fill>,
    pub total_orders: usize,
    pub total_positions: usize,
    /// PnL statistics in the quote currency (e.g. "PnL (total)", "Win Rate").
    pub pnl: BTreeMap<String, f64>,
    /// Return statistics (e.g. "Sharpe Ratio (252 days)", "Max Drawdown").
    pub returns: BTreeMap<String, f64>,
    /// General statistics (e.g. "Long Ratio").
    pub general: BTreeMap<String, f64>,
    /// Per-period returns, for the equity curve.
    pub returns_series: Vec<(UnixNanos, f64)>,
}

pub(crate) fn build(result: &BacktestResult, fills: Vec<Fill>, quote: &str) -> BacktestReport {
    BacktestReport {
        fills,
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
    }
}

fn sorted<'a>(stats: impl IntoIterator<Item = (&'a String, &'a f64)>) -> BTreeMap<String, f64> {
    stats.into_iter().map(|(k, v)| (k.clone(), *v)).collect()
}
