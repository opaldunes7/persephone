//! What a backtest comes to beyond its report: the figures Lab's summaries and the app's statistics
//! both show, worked out here so they can't drift apart.

use std::collections::BTreeMap;

use persephone_core::{Side, time::civil_from_days};
use persephone_engine::{BacktestReport, BacktestRequest, FeeRates, SimSettings};

use crate::robustness::round_trips;

const NANOS_PER_DAY: u64 = 86_400_000_000_000;

/// What the market did from the first close that could trade to the last.
#[must_use]
pub fn buy_and_hold(bars: &[(u64, f64)], from_index: usize) -> Option<f64> {
    let first = bars.get(from_index)?.1;
    let last = bars.last()?.1;
    (first > 0.0).then(|| last / first - 1.0)
}

/// Buying at the first close that could trade and holding to the end, with no costs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Benchmark {
    pub return_: f64,
    pub sharpe: Option<f64>,
    pub max_drawdown: f64,
}

/// The same figures a run gets, to compare it with.
#[must_use]
pub fn benchmark(capital: f64, bars: &[(u64, f64)], from_index: usize) -> Option<Benchmark> {
    let span = bars.get(from_index..)?;
    let first = span.first().map(|(_, close)| *close).filter(|c| *c > 0.0)?;
    let mut peak = 0.0_f64;
    let mut fall = 0.0_f64;
    for &(_, close) in span {
        peak = peak.max(close);
        fall = fall.max(1.0 - close / peak);
    }
    Some(Benchmark {
        return_: buy_and_hold(bars, from_index)?,
        sharpe: daily_sharpe(
            capital,
            span.iter()
                .map(|&(ts, close)| (ts, capital * close / first)),
        ),
        max_drawdown: fall,
    })
}

/// A run's Sharpe ratio, on the same terms as [`daily_sharpe`].
#[must_use]
pub fn sharpe(report: &BacktestReport, from_index: usize, bars: &[(u64, f64)]) -> Option<f64> {
    let points = bars.iter().zip(&report.equity).skip(from_index);
    daily_sharpe(
        report.starting_balance,
        points.map(|(&(ts, _), &equity)| (ts, equity)),
    )
}

/// The Sharpe ratio of daily returns, with no risk-free rate, over 365 days a year as crypto trades
/// every day. A day's value is its last bar's equity. `None` if there are too few days to say, or
/// nothing ever moved.
fn daily_sharpe(start: f64, points: impl Iterator<Item = (u64, f64)>) -> Option<f64> {
    let mut days = vec![start];
    let mut last_day = None;
    for (ts, equity) in points {
        let day = ts / NANOS_PER_DAY;
        if last_day == Some(day) {
            *days.last_mut()? = equity;
        } else {
            days.push(equity);
            last_day = Some(day);
        }
    }
    let returns: Vec<f64> = days
        .windows(2)
        .filter(|pair| pair[0] > 0.0)
        .map(|pair| pair[1] / pair[0] - 1.0)
        .collect();
    if returns.len() < 2 {
        return None;
    }
    let count = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / count;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (count - 1.0);
    (variance > 0.0).then(|| mean / variance.sqrt() * 365.0_f64.sqrt())
}

/// The share of the span's bars spent in a position.
#[must_use]
pub fn exposure(report: &BacktestReport, from_index: usize) -> f64 {
    let bars = report.equity.len().saturating_sub(from_index);
    if bars == 0 {
        return 0.0;
    }
    let mut position = 0.0;
    let mut fills = report.fills.iter().peekable();
    let mut held = 0;
    for bar in from_index..report.equity.len() {
        while let Some(fill) = fills.next_if(|fill| fill.bar < bar) {
            position += if fill.side == Side::Buy {
                fill.qty
            } else {
                -fill.qty
            };
        }
        // Rounding can leave dust where a position closed.
        if f64::abs(position) > 1e-12 {
            held += 1;
        }
    }
    f64::from(held) / bars as f64
}

/// What each UTC year added, as a fraction of the starting capital, so they add up to the net return.
#[must_use]
pub fn by_year(
    report: &BacktestReport,
    from_index: usize,
    bars: &[(u64, f64)],
) -> BTreeMap<i64, f64> {
    let capital = report.starting_balance;
    let mut years = BTreeMap::new();
    let mut before = capital;
    for (&(ts, _), &equity) in bars.iter().zip(&report.equity).skip(from_index) {
        let (year, _, _) = civil_from_days((ts / NANOS_PER_DAY) as i64);
        *years.entry(year).or_insert(0.0) += (equity - before) / capital;
        before = equity;
    }
    years
}

/// Winnings over losses across a run's trades, if it has any losses.
#[must_use]
pub fn profit_factor(report: &BacktestReport) -> Option<f64> {
    let trades = round_trips(report);
    let gains: f64 = trades.iter().map(|t| t.profit.max(0.0)).sum();
    let losses: f64 = trades.iter().map(|t| (-t.profit).max(0.0)).sum();
    (losses > 0.0).then(|| gains / losses)
}

/// Said when a logic tree's rules can't be true for at least 5% of the span, which trades or not is
/// easy to miss, as a rule that isn't ready is just false. Less is usual and not worth saying.
#[must_use]
pub fn look_back_warning(look_back: usize, before: usize, bars: usize) -> Option<String> {
    let unready = look_back.saturating_sub(before);
    let span = bars.saturating_sub(before);
    (unready > 0 && unready * 20 >= span).then(|| {
        format!(
            "A look-back here needs about {look_back} bars, with {before} before the span, so rules on it can't be true for about its first {unready} bars of {span}."
        )
    })
}

/// What costs did to a run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Friction {
    /// The return with no fees, slippage or spread.
    pub gross_return: f64,
    /// Fees plus what slippage and spread took, in the quote currency.
    pub costs_paid: f64,
    /// The cost per fill that would bring the costless run to nothing, in basis points of the value
    /// traded. `None` if it traded nothing.
    pub break_even_bp: Option<f64>,
}

/// `report`'s friction, given the same run with no costs, which is `report` itself if it had none.
#[must_use]
pub fn friction(
    report: &BacktestReport,
    free: &BacktestReport,
    sim: &SimSettings,
    price_precision: u8,
) -> Friction {
    Friction {
        gross_return: free
            .equity
            .last()
            .map_or(0.0, |last| last / free.starting_balance - 1.0),
        costs_paid: costs_paid(report, sim, price_precision),
        break_even_bp: break_even_bp(free),
    }
}

/// Whether a backtest has anything to pay: fees, slippage or spread.
#[must_use]
pub fn costs_something(request: &BacktestRequest) -> bool {
    let sim = &request.sim;
    request.fees.maker != 0.0
        || request.fees.taker != 0.0
        || sim.fee_per_order.is_some()
        || sim.slippage_ticks > 0
        || sim.slippage_percent > 0.0
        || sim.spread_ticks > 0
}

/// The same backtest with nothing to pay.
#[must_use]
pub fn without_costs(request: &BacktestRequest) -> BacktestRequest {
    let mut free = request.clone();
    free.fees = FeeRates::ZERO;
    free.sim.fee_per_order = None;
    free.sim.slippage_ticks = 0;
    free.sim.slippage_percent = 0.0;
    free.sim.spread_ticks = 0;
    free
}

/// Fees plus what slippage and spread took from each fill, worked back from its price as the fill
/// model moved it. A market's own quotes bring their own spread, which isn't known here.
fn costs_paid(report: &BacktestReport, sim: &SimSettings, price_precision: u8) -> f64 {
    let tick = 10_f64.powi(-i32::from(price_precision));
    let share = sim.slippage_percent / 100.0;
    let moved: f64 = report
        .fills
        .iter()
        .map(|fill| {
            let (spread, sign) = if fill.side == Side::Buy {
                (sim.spread_ticks.div_ceil(2), 1.0)
            } else {
                (sim.spread_ticks / 2, -1.0)
            };
            let ticks = f64::from(sim.slippage_ticks + spread) * tick;
            let met = (fill.price - sign * ticks) / (1.0 + sign * share);
            fill.qty * (fill.price - met) * sign
        })
        .sum();
    report.fees + moved
}

/// What a backtest with no costs made, per fill, as basis points of the value traded.
fn break_even_bp(free: &BacktestReport) -> Option<f64> {
    let traded: f64 = free.fills.iter().map(|fill| fill.qty * fill.price).sum();
    (traded > 0.0).then(|| free.net_pnl() / traded * 10_000.0)
}

/// A finished run's figures, to show beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct RunFigures {
    /// Flat to flat, so a reversal is two. Not fills.
    pub trades: usize,
    pub win_rate: Option<f64>,
    pub sharpe: Option<f64>,
    pub profit_factor: Option<f64>,
    pub exposure: f64,
    pub by_year: BTreeMap<i64, f64>,
    pub benchmark: Option<Benchmark>,
}

impl RunFigures {
    #[must_use]
    pub fn of(report: &BacktestReport, from_index: usize, bars: &[(u64, f64)]) -> Self {
        let trades = round_trips(report);
        let winners = trades.iter().filter(|t| t.profit > 0.0).count();
        Self {
            trades: trades.len(),
            win_rate: (!trades.is_empty()).then(|| winners as f64 / trades.len() as f64),
            sharpe: sharpe(report, from_index, bars),
            profit_factor: profit_factor(report),
            exposure: exposure(report, from_index),
            by_year: by_year(report, from_index, bars),
            benchmark: benchmark(report.starting_balance, bars, from_index),
        }
    }
}
