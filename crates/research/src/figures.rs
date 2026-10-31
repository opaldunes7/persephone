//! What a backtest comes to beyond its report: the figures Lab's summaries and the app's statistics
//! both show, worked out here so they can't drift apart.

use std::collections::BTreeMap;

use persephone_core::{Side, time::civil_from_days};
use persephone_engine::{
    BacktestReport, BacktestRequest, FeeRates, LimitOutcome, SimSettings, impact_share,
};

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
    /// The return once a perpetual's funding has been paid on it. What holding one really made.
    pub return_after_funding: f64,
}

/// The same figures a run gets, to compare it with.
#[must_use]
pub fn benchmark(
    capital: f64,
    bars: &[(u64, f64)],
    from_index: usize,
    funding: &[(u64, f64)],
) -> Option<Benchmark> {
    let span = bars.get(from_index..)?;
    let first = span.first().map(|(_, close)| *close).filter(|c| *c > 0.0)?;
    let mut peak = 0.0_f64;
    let mut fall = 0.0_f64;
    for &(_, close) in span {
        peak = peak.max(close);
        fall = fall.max(1.0 - close / peak);
    }
    // As a share of the capital: the position's value when each falls due, times its rate.
    let paid: f64 = funding
        .iter()
        .filter_map(|&(when, rate)| {
            let at = span.partition_point(|bar| bar.0 <= when).checked_sub(1)?;
            Some(span[at].1 / first * rate)
        })
        .sum();
    let return_ = buy_and_hold(bars, from_index)?;
    Some(Benchmark {
        return_,
        return_after_funding: return_ - paid,
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
    /// Fees, slippage, spread, impact, funding and borrowing, in the quote currency.
    pub costs_paid: f64,
    /// The fees on orders that rested, below zero if they earned a rebate, and on those that took.
    pub maker_fees: f64,
    pub taker_fees: f64,
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
    volumes: &[f64],
) -> Friction {
    let fees_of = |maker: bool| -> f64 {
        report
            .fills
            .iter()
            .filter(|fill| fill.maker == maker)
            .map(|fill| fill.fee)
            .sum::<f64>()
            + 0.0
    };
    Friction {
        maker_fees: fees_of(true),
        taker_fees: fees_of(false),
        gross_return: free
            .equity
            .last()
            .map_or(0.0, |last| last / free.starting_balance - 1.0),
        costs_paid: costs_paid(report, sim, price_precision, volumes),
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
        || sim.impact_percent > 0.0
        || sim.borrow_percent_per_year.is_some()
        || request
            .funding
            .as_ref()
            .is_some_and(|rates| !rates.is_empty())
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
    free.sim.impact_percent = 0.0;
    free.sim.borrow_percent_per_year = None;
    free.funding = None;
    free
}

/// All a run paid: fees, funding and borrowing as they were charged, and what slippage, spread and
/// impact took from each order that didn't rest, worked back from its price as the fill model moved
/// it. A market's own quotes bring their own spread, which isn't known here.
fn costs_paid(
    report: &BacktestReport,
    sim: &SimSettings,
    price_precision: u8,
    volumes: &[f64],
) -> f64 {
    let tick = 10_f64.powi(-i32::from(price_precision));
    let moved: f64 = report
        .fills
        .iter()
        .filter(|fill| !fill.maker)
        .map(|fill| {
            let (spread, sign) = if fill.side == Side::Buy {
                (sim.spread_ticks.div_ceil(2), 1.0)
            } else {
                (sim.spread_ticks / 2, -1.0)
            };
            let volume = volumes.get(fill.bar).copied().unwrap_or(0.0);
            let share =
                sim.slippage_percent / 100.0 + impact_share(sim.impact_percent, fill.qty, volume);
            let ticks = f64::from(sim.slippage_ticks + spread) * tick;
            let met = (fill.price - sign * ticks) / (1.0 + sign * share);
            fill.qty * (fill.price - met) * sign
        })
        .sum();
    report.fees + moved + report.funding_paid + report.borrow_paid
}

/// What a backtest with no costs made, per fill, as basis points of the value traded.
fn break_even_bp(free: &BacktestReport) -> Option<f64> {
    let traded: f64 = free.fills.iter().map(|fill| fill.qty * fill.price).sum();
    (traded > 0.0).then(|| free.net_pnl() / traded * 10_000.0)
}

/// What the account would be worth holding the market with `share` of the capital since `first`: the
/// same size of position a strategy takes, so the two can be compared on one scale.
#[must_use]
pub fn market_equity(capital: f64, share: f64, first: f64, close: f64) -> f64 {
    capital * (1.0 + share * (close / first - 1.0))
}

/// How many bars a chart of a spread has, in the app and from Lab alike.
pub const HISTOGRAM_BINS: usize = 40;

/// How values are spread: `counts[i]` of them lie in the `i`th of equal steps from `lo` to `hi`.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub lo: f64,
    pub hi: f64,
    pub counts: Vec<usize>,
}

impl Histogram {
    /// Over `bins` steps, wide enough to reach `also`, so a mark there is always on the chart.
    #[must_use]
    pub fn of(values: &[f64], bins: usize, also: f64) -> Self {
        let bins = bins.max(1);
        let (lo, hi) = values
            .iter()
            .chain(std::iter::once(&also))
            .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
        // All the same still needs some width to draw.
        let (lo, hi) = if hi > lo {
            (lo, hi)
        } else {
            (lo - 0.005, hi + 0.005)
        };
        let mut counts = vec![0; bins];
        for value in values {
            let at = ((value - lo) / (hi - lo) * bins as f64) as usize;
            counts[at.min(bins - 1)] += 1;
        }
        Self { lo, hi, counts }
    }

    /// Where `value` lies across it, from 0 to 1.
    #[must_use]
    pub fn share_of(&self, value: f64) -> f64 {
        ((value - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
    }
}

/// How limit orders that filled did against those that missed: whether the market fills the bad ones.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LimitSummary {
    pub filled: usize,
    pub missed: usize,
    /// Fills that went less than a hundredth of a percent past their price: the ones most likely not
    /// to have filled at all, behind others in the queue.
    pub barely_filled: usize,
    /// How far the market moved each order's way, on average, as percentages, from the close it went
    /// out at. `None` without any to go on.
    pub filled_move_1: Option<f64>,
    pub missed_move_1: Option<f64>,
    pub filled_move_24: Option<f64>,
    pub missed_move_24: Option<f64>,
}

impl LimitSummary {
    #[must_use]
    pub fn of(report: &BacktestReport) -> Self {
        let log = &report.limit_log;
        let is_fill = |outcome: LimitOutcome| outcome == LimitOutcome::Filled;
        let is_miss = |outcome: LimitOutcome| {
            matches!(outcome, LimitOutcome::Lapsed | LimitOutcome::Replaced)
        };
        let average = |wanted: &dyn Fn(LimitOutcome) -> bool, bars_on: bool| {
            let moves: Vec<f64> = log
                .iter()
                .filter(|row| wanted(row.outcome))
                .filter_map(|row| {
                    if bars_on {
                        row.move_24_percent
                    } else {
                        row.move_1_percent
                    }
                })
                .collect();
            (!moves.is_empty()).then(|| moves.iter().sum::<f64>() / moves.len() as f64)
        };
        Self {
            filled: log.iter().filter(|row| is_fill(row.outcome)).count(),
            missed: log.iter().filter(|row| is_miss(row.outcome)).count(),
            barely_filled: log
                .iter()
                .filter(|row| is_fill(row.outcome) && row.reached_percent < 0.01)
                .count(),
            filled_move_1: average(&is_fill, false),
            missed_move_1: average(&is_miss, false),
            filled_move_24: average(&is_fill, true),
            missed_move_24: average(&is_miss, true),
        }
    }

    /// Said when orders that missed did clearly better than those that filled, whatever the fill rate:
    /// a result built on the rest is being paid to take the bad side.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        let (filled, missed) = (self.filled_move_1?, self.missed_move_1?);
        (self.filled >= 20 && self.missed >= 20 && missed - filled >= 0.05).then(|| {
            format!(
                "Limit orders that missed moved {missed:+.2}% their way a bar later, and those that filled {filled:+.2}%. Fills are the worse side of the bargain here, so test with `through_percent`, and see `get_limit_orders`."
            )
        })
    }
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
    /// The share of limit orders that filled, if any went out.
    pub fill_rate: Option<f64>,
}

impl RunFigures {
    #[must_use]
    pub fn of(
        report: &BacktestReport,
        from_index: usize,
        bars: &[(u64, f64)],
        funding: &[(u64, f64)],
    ) -> Self {
        let trades = round_trips(report);
        let winners = trades.iter().filter(|t| t.profit > 0.0).count();
        Self {
            trades: trades.len(),
            win_rate: (!trades.is_empty()).then(|| winners as f64 / trades.len() as f64),
            sharpe: sharpe(report, from_index, bars),
            profit_factor: profit_factor(report),
            exposure: exposure(report, from_index),
            by_year: by_year(report, from_index, bars),
            benchmark: benchmark(report.starting_balance, bars, from_index, funding),
            fill_rate: (report.limit_orders > 0)
                .then(|| report.limits_filled as f64 / report.limit_orders as f64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_histogram_counts_everything_even_at_its_top_edge() {
        let values = [-0.1, 0.0, 0.0, 0.05, 0.1];
        let found = Histogram::of(&values, 4, 0.0);
        assert_eq!(found.counts.iter().sum::<usize>(), values.len());
        assert_eq!(
            *found.counts.last().unwrap(),
            2,
            "the top one is in the last"
        );
        assert_eq!((found.lo, found.hi), (-0.1, 0.1));
        assert!((found.share_of(0.0) - 0.5).abs() < 1e-12);

        // A mark beyond every value widens it.
        let found = Histogram::of(&values, 4, 0.3);
        assert_eq!(found.hi, 0.3);
        assert_eq!(found.share_of(0.3), 1.0);

        // All the same doesn't divide by nothing.
        let found = Histogram::of(&[0.2, 0.2], 4, 0.2);
        assert_eq!(found.counts.iter().sum::<usize>(), 2);
        assert!(found.hi > found.lo);
    }

    #[test]
    fn holding_a_perpetual_pays_its_funding_at_what_it_was_worth() {
        const DAY: u64 = 86_400_000_000_000;
        let bars = [(0, 100.0), (DAY, 200.0), (2 * DAY, 200.0)];
        // Half a percent while at 100, and 1% while at twice that, on the same position.
        let funding = [(10, 0.005), (DAY + 10, 0.01)];
        let held = benchmark(1_000.0, &bars, 0, &funding).unwrap();
        assert!((held.return_ - 1.0).abs() < 1e-12);
        assert!((held.return_after_funding - (1.0 - 0.005 - 0.02)).abs() < 1e-12);
        assert_eq!(
            benchmark(1_000.0, &bars, 0, &[])
                .unwrap()
                .return_after_funding,
            1.0
        );
    }

    #[test]
    fn the_market_line_holds_the_same_size_as_the_strategy() {
        // A tenth of 1,000 in a market up from 100 to 150 makes 50.
        assert!((market_equity(1_000.0, 0.1, 100.0, 150.0) - 1_050.0).abs() < 1e-9);
        assert!((market_equity(1_000.0, 1.0, 100.0, 50.0) - 500.0).abs() < 1e-9);
    }
}
