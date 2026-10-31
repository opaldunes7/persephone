//! Two tests of whether a backtest's result was skill or luck, beyond the one timeline it ran.
//!
//! Monte Carlo takes the trades the backtest made and draws them again at random, many times, to
//! see what else the same trades could have added up to. Random blocks reruns the strategy over
//! random stretches of the market, to see whether it only worked in one of them.

use std::{
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use persephone_core::{Candle, Side, UnixNanos, time::NANOS_PER_DAY};
use persephone_data::binance::MAX_RANGE_BARS;
use persephone_engine::{BacktestReport, BacktestRequest};
use rand::{RngExt, SeedableRng, rngs::StdRng};

use crate::{RunColumn, WARM_UP_BARS, settings::BlockLength};

/// Fewer trades than this say too little to draw from.
pub const MIN_TRADES: usize = 10;
/// The most Monte Carlo runs drawn as lines; the figures use them all.
const MAX_PATHS: usize = 200;
/// The most points on a drawn line.
const MAX_POINTS: usize = 500;

/// One trade from flat to flat, with a reversal counted as a close and an open.
#[derive(Clone, Debug, PartialEq)]
pub struct RoundTrip {
    pub long: bool,
    /// The most it held.
    pub qty: f64,
    pub entry_ts: UnixNanos,
    /// The bar whose close started it.
    pub entry_bar: usize,
    /// What it paid on average to get in, and got on average to get out.
    pub entry_price: f64,
    pub exit_ts: UnixNanos,
    pub exit_bar: usize,
    pub exit_price: f64,
    /// What it came to, after fees.
    pub profit: f64,
    pub fees: f64,
    /// Still open at the end, so its profit is at the last close and it has no exit of its own.
    pub open: bool,
}

/// The trades the backtest made, in the order they started. One still open at the end counts at
/// the last close, as the report's equity does, so they add up to the backtest's whole profit.
#[must_use]
pub fn round_trips(report: &BacktestReport) -> Vec<RoundTrip> {
    let mut trades: Vec<RoundTrip> = Vec::new();
    let mut current: Option<RoundTrip> = None;
    let mut position = 0.0_f64;
    // What the position cost, signed like it.
    let mut cost = 0.0_f64;
    // For the average prices: what was bought or sold on the way in and out.
    let (mut opened, mut opened_value, mut closed, mut closed_value) = (0.0, 0.0, 0.0, 0.0);
    for fill in report.fills.iter().filter(|f| f.qty > 0.0) {
        let sign = if fill.side == Side::Buy { 1.0 } else { -1.0 };
        let closing = if position * sign < 0.0 {
            fill.qty.min(position.abs())
        } else {
            0.0
        };
        if closing > 0.0
            && let Some(trade) = current.as_mut()
        {
            let entry = cost / position;
            let fee = fill.fee * closing / fill.qty;
            trade.profit += closing * (fill.price - entry) * position.signum() - fee;
            trade.fees += fee;
            cost -= closing * entry * position.signum();
            position += closing * sign;
            closed += closing;
            closed_value += closing * fill.price;
            trade.exit_ts = fill.ts;
            trade.exit_bar = fill.bar;
            trade.exit_price = closed_value / closed;
            if position.abs() <= fill.qty * 1e-9 {
                trade.open = false;
                trades.extend(current.take());
                (position, cost) = (0.0, 0.0);
                (opened, opened_value, closed, closed_value) = (0.0, 0.0, 0.0, 0.0);
            }
        }
        let opening = fill.qty - closing;
        if opening > fill.qty * 1e-9 {
            let fee = fill.fee * opening / fill.qty;
            position += opening * sign;
            cost += opening * sign * fill.price;
            opened += opening;
            opened_value += opening * fill.price;
            let trade = current.get_or_insert(RoundTrip {
                long: sign > 0.0,
                qty: 0.0,
                entry_ts: fill.ts,
                entry_bar: fill.bar,
                entry_price: fill.price,
                exit_ts: fill.ts,
                exit_bar: fill.bar,
                exit_price: fill.price,
                profit: 0.0,
                fees: 0.0,
                open: true,
            });
            trade.qty = trade.qty.max(position.abs());
            trade.entry_price = opened_value / opened;
            trade.profit -= fee;
            trade.fees += fee;
        }
    }
    if let Some(mut trade) = current {
        let last = report
            .equity
            .last()
            .copied()
            .unwrap_or(report.starting_balance);
        let before: f64 = trades.iter().map(|t| t.profit).sum();
        trade.profit = last - report.starting_balance - before;
        trade.exit_bar = report.equity.len().saturating_sub(1);
        trades.push(trade);
    }
    trades
}

/// What each trade the backtest made came to, after fees.
#[must_use]
pub fn closed_trades(report: &BacktestReport) -> Vec<f64> {
    round_trips(report).iter().map(|t| t.profit).collect()
}

/// A backtest's result, to draw at random off the UI thread.
pub struct MonteCarloJob {
    pub report: Arc<BacktestReport>,
    pub runs: u32,
    /// The fall from a peak that counts as ruin, as a fraction.
    pub ruin_at: f64,
    pub seed: u64,
}

impl MonteCarloJob {
    /// # Errors
    ///
    /// Returns a message if there are too few trades to say anything.
    pub fn run(&self) -> Result<MonteCarlo, String> {
        monte_carlo(
            &closed_trades(&self.report),
            self.report.starting_balance,
            self.runs,
            self.ruin_at,
            self.seed,
        )
    }
}

/// What many random draws of a backtest's trades came to.
#[derive(Clone, Debug)]
pub struct MonteCarlo {
    pub trades: usize,
    pub runs: u32,
    /// Returns as fractions of the capital: the median, and the 5th and 95th percentiles.
    pub median: f64,
    pub low: f64,
    pub high: f64,
    /// The fall from a peak that only one run in twenty was worse than, as a fraction.
    pub drawdown: f64,
    /// The share of runs that fell at least the ruin level from a peak.
    pub ruin: f64,
    /// Some of the runs' equity, from the capital on, always with the best, worst and middle.
    pub paths: Vec<Vec<f64>>,
    /// How each of those runs went.
    pub stats: Vec<LineStats>,
    /// Which of them ended highest, lowest and nearest the median.
    pub best: usize,
    pub worst: usize,
    pub middle: usize,
    pub median_path: Vec<f64>,
    /// The trades as they really came, and how that went.
    pub actual: Vec<f64>,
    pub actual_stats: LineStats,
    /// How every run went, drawn as a line or not.
    pub all_stats: Vec<LineStats>,
    /// The fall that counts as ruin, as a fraction.
    pub ruin_at: f64,
    /// Which run each line is, and the seed, to work out again what a run drew.
    kept_runs: Vec<usize>,
    seed: u64,
    /// Trades between a line's points.
    pub stride: usize,
}

impl MonteCarlo {
    /// Which of the backtest's trades a run drew, in order, by their place among them. The run
    /// after the last is the backtest itself, with each trade once as it came. Drawing with
    /// replacement, a run can have the same trade twice and leave others out.
    #[must_use]
    pub fn draws_of_run(&self, run: usize) -> Option<Vec<usize>> {
        if run == self.all_stats.len() {
            return Some((0..self.trades).collect());
        }
        (run < self.all_stats.len()).then(|| {
            let mut rng = run_rng(self.seed, run);
            (0..self.trades)
                .map(|_| rng.random_range(0..self.trades))
                .collect()
        })
    }

    /// How a run went; the run after the last is the backtest itself.
    #[must_use]
    pub fn stats_of_run(&self, run: usize) -> Option<LineStats> {
        if run == self.all_stats.len() {
            return Some(self.actual_stats);
        }
        self.all_stats.get(run).copied()
    }

    /// Which run a line is; the line after the last is the backtest itself.
    #[must_use]
    pub fn run_of_line(&self, line: usize) -> Option<usize> {
        if line == self.paths.len() {
            return Some(self.all_stats.len());
        }
        self.kept_runs.get(line).copied()
    }

    /// Which line a run is, if it's one of those drawn.
    #[must_use]
    pub fn line_of_run(&self, run: usize) -> Option<usize> {
        if run == self.all_stats.len() {
            return Some(self.paths.len());
        }
        self.kept_runs.iter().position(|&kept| kept == run)
    }

    /// Every run and the backtest itself, best return first.
    #[must_use]
    pub fn ranked_runs(&self) -> Vec<RunRow> {
        let mut rows: Vec<RunRow> = self
            .all_stats
            .iter()
            .copied()
            .chain(std::iter::once(self.actual_stats))
            .enumerate()
            .map(|(run, stats)| RunRow {
                rank: 0,
                run,
                stats,
                backtest: run == self.all_stats.len(),
            })
            .collect();
        rows.sort_by(|a, b| {
            b.stats
                .return_
                .total_cmp(&a.stats.return_)
                .then(a.run.cmp(&b.run))
        });
        for (place, row) in rows.iter_mut().enumerate() {
            row.rank = place + 1;
        }
        rows
    }
}

/// `rows` as the table of runs shows them, sorted by `column`, the largest first if `descending`.
pub fn sort_rows(rows: &mut [RunRow], (column, descending): (RunColumn, bool)) {
    rows.sort_by(|a, b| {
        let order = match column {
            RunColumn::Return => a.stats.return_.total_cmp(&b.stats.return_),
            RunColumn::Fell => a.stats.drawdown.total_cmp(&b.stats.drawdown),
            RunColumn::Run => a.run.cmp(&b.run),
        };
        if descending { order.reverse() } else { order }
    });
}

/// One row of the table of runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunRow {
    /// Its place by return, from 1.
    pub rank: usize,
    pub run: usize,
    pub stats: LineStats,
    /// It's the backtest itself, not a draw.
    pub backtest: bool,
}

/// What a run's trades came to, for its table of figures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunSummary {
    pub trades: usize,
    /// How many of the backtest's trades it drew at least once.
    pub different: usize,
    pub winners: usize,
    pub best: f64,
    pub worst: f64,
    pub average: f64,
    /// The most losing trades in a row.
    pub losing_streak: usize,
}

/// Sums up what a run drew, by their places in `profits`.
#[must_use]
pub fn summarize_run(draws: &[usize], profits: &[f64]) -> RunSummary {
    let drawn: Vec<f64> = draws
        .iter()
        .filter_map(|&i| profits.get(i).copied())
        .collect();
    let mut seen = draws.to_vec();
    seen.sort_unstable();
    seen.dedup();
    let (mut streak, mut losing_streak) = (0, 0);
    for profit in &drawn {
        streak = if *profit < 0.0 { streak + 1 } else { 0 };
        losing_streak = losing_streak.max(streak);
    }
    RunSummary {
        trades: drawn.len(),
        different: seen.len(),
        winners: drawn.iter().filter(|p| **p > 0.0).count(),
        best: drawn.iter().copied().fold(f64::MIN, f64::max),
        worst: drawn.iter().copied().fold(f64::MAX, f64::min),
        average: drawn.iter().sum::<f64>() / drawn.len().max(1) as f64,
        losing_streak,
    }
}

/// The random numbers of one run: its own, so what it drew can be worked out again later without
/// keeping every run's draws. `Run again`'s next seed shares none with this one's.
fn run_rng(seed: u64, run: usize) -> StdRng {
    StdRng::seed_from_u64((seed << 32) ^ run as u64)
}

/// How one line on a fan went.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineStats {
    /// As a fraction of the capital.
    pub return_: f64,
    /// The deepest fall from a peak, as a fraction of that peak.
    pub drawdown: f64,
}

/// The deepest fall from a peak along `equity`, from `capital`.
fn deepest_fall(equity: impl Iterator<Item = f64>, capital: f64) -> f64 {
    let mut peak = capital;
    let mut fall = 0.0_f64;
    for value in equity {
        peak = peak.max(value);
        fall = fall.max(1.0 - value / peak);
    }
    fall.min(1.0)
}

/// Runs `runs` draws of as many trades as `trades` has, with replacement, so that both where a
/// run ends and how it gets there vary (shuffling would only change the way there). Adding the
/// profits up is fair because positions are sized from the starting capital, not compounded.
///
/// # Errors
///
/// Returns a message if there are too few trades to say anything.
pub fn monte_carlo(
    trades: &[f64],
    capital: f64,
    runs: u32,
    ruin_at: f64,
    seed: u64,
) -> Result<MonteCarlo, String> {
    if trades.len() < MIN_TRADES {
        return Err(format!(
            "Monte Carlo needs at least {MIN_TRADES} trades to draw from; this backtest made {}. \
             Try a wider span.",
            trades.len()
        ));
    }
    let stride = stride_for(trades.len() + 1);
    let mut returns = Vec::with_capacity(runs as usize);
    let mut drawdowns = Vec::with_capacity(runs as usize);
    let mut paths: Vec<Vec<f64>> = Vec::new();
    let mut ruined = 0_u32;
    for run in 0..runs as usize {
        let mut rng = run_rng(seed, run);
        let mut path = vec![capital];
        let (mut equity, mut peak, mut fall) = (capital, capital, 0.0_f64);
        for i in 1..=trades.len() {
            equity += trades[rng.random_range(0..trades.len())];
            peak = peak.max(equity);
            fall = fall.max(1.0 - equity / peak);
            if i % stride == 0 {
                path.push(equity);
            }
        }
        returns.push(equity / capital - 1.0);
        drawdowns.push(fall.min(1.0));
        ruined += u32::from(fall >= ruin_at);
        paths.push(path);
    }
    let in_order: Vec<LineStats> = returns
        .iter()
        .zip(&drawdowns)
        .map(|(&return_, &drawdown)| LineStats { return_, drawdown })
        .collect();
    returns.sort_by(f64::total_cmp);
    drawdowns.sort_by(f64::total_cmp);
    // The median line is every run's, so it ends where the median figure says.
    let median_line = median_path(&paths);
    let median = percentile(&returns, 0.5);
    let closest = |better: &dyn Fn(f64, f64) -> bool| {
        (0..in_order.len()).fold(0, |found, run| {
            if better(in_order[run].return_, in_order[found].return_) {
                run
            } else {
                found
            }
        })
    };
    let extremes = [
        closest(&|a, b| a > b),
        closest(&|a, b| a < b),
        closest(&|a, b| (a - median).abs() < (b - median).abs()),
    ];
    // Draw the first few runs, swapping in those three if they aren't among them.
    let mut kept: Vec<usize> = (0..in_order.len().min(MAX_PATHS)).collect();
    let mut spare = kept.len();
    for extreme in extremes {
        if !kept.contains(&extreme) {
            spare -= 1;
            while extremes.contains(&kept[spare]) {
                spare -= 1;
            }
            kept[spare] = extreme;
        }
    }
    let place = |run: usize| kept.iter().position(|&k| k == run).unwrap_or(0);
    let (best, worst, middle) = (place(extremes[0]), place(extremes[1]), place(extremes[2]));
    let stats = kept.iter().map(|&run| in_order[run]).collect();
    let kept_runs = kept.clone();
    let paths = kept
        .iter()
        .map(|&run| std::mem::take(&mut paths[run]))
        .collect();
    // The trades as they came, for comparing the draws with what really happened.
    let mut running = capital;
    let real: Vec<f64> = std::iter::once(capital)
        .chain(trades.iter().map(|trade| {
            running += trade;
            running
        }))
        .collect();
    let actual_stats = LineStats {
        return_: running / capital - 1.0,
        drawdown: deepest_fall(real.iter().copied(), capital),
    };
    let actual = real.into_iter().step_by(stride).collect();
    Ok(MonteCarlo {
        trades: trades.len(),
        runs,
        median,
        low: percentile(&returns, 0.05),
        high: percentile(&returns, 0.95),
        drawdown: percentile(&drawdowns, 0.95),
        ruin: f64::from(ruined) / f64::from(runs.max(1)),
        median_path: median_line,
        paths,
        stats,
        best,
        worst,
        middle,
        actual,
        actual_stats,
        all_stats: in_order,
        ruin_at,
        kept_runs,
        seed,
        stride,
    })
}

/// Every how many points to keep, for a line of `points` to fit within [`MAX_POINTS`].
fn stride_for(points: usize) -> usize {
    points.div_ceil(MAX_POINTS).max(1)
}

/// The value `share` of the way along `sorted`, between neighbours if it falls between them.
pub(super) fn percentile(sorted: &[f64], share: f64) -> f64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return 0.0;
    };
    let at = share * last as f64;
    let below = at.floor() as usize;
    let above = at.ceil() as usize;
    sorted[below] + (sorted[above] - sorted[below]) * (at - below as f64)
}

/// The middle of `paths` at each point, among those long enough to reach it.
fn median_path(paths: &[Vec<f64>]) -> Vec<f64> {
    let longest = paths.iter().map(Vec::len).max().unwrap_or(0);
    let mut values = Vec::with_capacity(paths.len());
    (0..longest)
        .map(|i| {
            values.clear();
            values.extend(paths.iter().filter_map(|path| path.get(i)));
            values.sort_by(f64::total_cmp);
            percentile(&values, 0.5)
        })
        .collect()
}

/// Why a test can't run yet, and how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub message: String,
    /// Widening the span would fix it, so the panel offers to.
    pub widen: bool,
}

impl Problem {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            widen: false,
        }
    }
}

/// Why blocks of `length` can't be picked from bar `from` on, if they can't.
#[must_use]
pub fn blocks_problem(candles: &[Candle], from: usize, length: BlockLength) -> Option<Problem> {
    let span = candles.get(from..).unwrap_or_default();
    let (Some(first), Some(last)) = (span.first(), span.last()) else {
        return Some(Problem::new("There are no bars to pick blocks from."));
    };
    let (name, nanos) = (length.name(), length.nanos());
    if last.ts - first.ts < nanos {
        let days = (last.ts - first.ts) / NANOS_PER_DAY;
        let covers = match days {
            0 => "less than a day".to_owned(),
            1 => "1 day".to_owned(),
            days => format!("{days} days"),
        };
        return Some(Problem {
            message: format!(
                "The span covers {covers}, and {name} blocks need {}. Widen it, or pick shorter \
                 blocks.",
                nanos / NANOS_PER_DAY
            ),
            widen: true,
        });
    }
    if span.partition_point(|c| c.ts < first.ts + nanos) < 2 {
        return Some(Problem::new(format!(
            "{name} blocks hold too few bars on this timeframe. Pick longer blocks."
        )));
    }
    None
}

/// A span reaching back from `last` far enough for blocks of `needed`, within the market's
/// history and what can be loaded at once: four blocks' worth if there's room.
///
/// # Errors
///
/// Returns a message if even one block's worth won't do.
pub fn widened_span(
    first: UnixNanos,
    last: UnixNanos,
    needed: UnixNanos,
    bar_nanos: UnixNanos,
) -> Result<(UnixNanos, UnixNanos), String> {
    let most = MAX_RANGE_BARS.saturating_sub(WARM_UP_BARS) * bar_nanos.max(1);
    if needed > most {
        return Err(
            "Blocks that long are more bars than can load at once. Use a higher \
                    timeframe, or pick shorter blocks."
                .into(),
        );
    }
    let from = last.saturating_sub((needed * 4).min(most)).max(first);
    if last - from < needed {
        return Err(
            "The market's whole history is shorter than one block. Pick shorter blocks.".into(),
        );
    }
    Ok((from, last))
}

/// `count` random stretches of `length` from bar `from` on, as ranges of `candles`. They can
/// overlap: each is drawn on its own, so a block's result says nothing about the next one's.
///
/// # Errors
///
/// Returns a message if the bars don't cover one block, or a block would hold too few bars.
pub fn pick_blocks(
    candles: &[Candle],
    from: usize,
    length: BlockLength,
    count: u32,
    seed: u64,
) -> Result<Vec<Range<usize>>, String> {
    if let Some(problem) = blocks_problem(candles, from, length) {
        return Err(problem.message);
    }
    let nanos = length.nanos();
    let last = candles[candles.len() - 1].ts;
    let last_start = candles.partition_point(|c| c.ts + nanos <= last) - 1;
    let mut rng = StdRng::seed_from_u64(seed);
    let blocks: Vec<_> = (0..count)
        .map(|_| {
            let start = rng.random_range(from..=last_start);
            let until = candles[start].ts + nanos;
            start..candles.partition_point(|c| c.ts < until)
        })
        .collect();
    if blocks.iter().any(|block| block.len() < 2) {
        return Err(format!(
            "{} blocks hold too few bars on this timeframe. Pick longer blocks.",
            length.name()
        ));
    }
    Ok(blocks)
}

/// How one block went.
#[derive(Clone, Debug)]
pub struct BlockResult {
    pub from: UnixNanos,
    pub to: UnixNanos,
    pub return_: f64,
    /// The deepest fall from a peak, as a fraction of that peak.
    pub drawdown: f64,
    /// How many orders filled.
    pub fills: usize,
    /// Its last bar on the chart, for showing it there.
    pub last_bar: usize,
    /// Equity at the block's bars' closes.
    pub path: Vec<f64>,
    /// What it traded, with bars counted as on the chart.
    pub trades: Vec<RoundTrip>,
}

/// How one block went, without its line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockStats {
    pub from: UnixNanos,
    pub to: UnixNanos,
    pub stats: LineStats,
    pub fills: usize,
    pub last_bar: usize,
}

impl BlockSummary {
    /// Every block, best return first, as rows of the table of runs.
    #[must_use]
    pub fn ranked(&self) -> Vec<RunRow> {
        let mut rows: Vec<RunRow> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(run, block)| RunRow {
                rank: 0,
                run,
                stats: block.stats,
                backtest: false,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.stats
                .return_
                .total_cmp(&a.stats.return_)
                .then(a.run.cmp(&b.run))
        });
        for (place, row) in rows.iter_mut().enumerate() {
            row.rank = place + 1;
        }
        rows
    }
}

/// How all the blocks went.
#[derive(Clone, Debug)]
pub struct BlockSummary {
    /// The share of blocks that made money.
    pub profitable: f64,
    /// Returns as fractions of the capital.
    pub median: f64,
    pub best: f64,
    pub worst: f64,
    /// When the worst block was.
    pub worst_dates: (UnixNanos, UnixNanos),
    /// Each block, in the same order as its line, and what each traded.
    pub blocks: Vec<BlockStats>,
    pub trades: Vec<Vec<RoundTrip>>,
    /// Which lines ended highest and lowest.
    pub best_line: usize,
    pub worst_line: usize,
    pub paths: Vec<Vec<f64>>,
    pub median_path: Vec<f64>,
    /// Bars between a line's points.
    pub stride: usize,
}

/// Sums up `results`, whose lines keep every `stride`th point.
#[must_use]
pub fn summarize(results: Vec<BlockResult>, stride: usize) -> BlockSummary {
    let mut returns: Vec<f64> = results.iter().map(|r| r.return_).collect();
    returns.sort_by(f64::total_cmp);
    let worst = results
        .iter()
        .min_by(|a, b| a.return_.total_cmp(&b.return_));
    let worst_dates = worst.map_or((0, 0), |r| (r.from, r.to));
    let line = |better: &dyn Fn(f64, f64) -> bool| {
        (0..results.len()).fold(0, |found, i| {
            if better(results[i].return_, results[found].return_) {
                i
            } else {
                found
            }
        })
    };
    let (best_line, worst_line) = (line(&|a, b| a > b), line(&|a, b| a < b));
    let blocks = results
        .iter()
        .map(|r| BlockStats {
            from: r.from,
            to: r.to,
            stats: LineStats {
                return_: r.return_,
                drawdown: r.drawdown,
            },
            fills: r.fills,
            last_bar: r.last_bar,
        })
        .collect();
    let (paths, trades): (Vec<Vec<f64>>, Vec<Vec<RoundTrip>>) =
        results.into_iter().map(|r| (r.path, r.trades)).unzip();
    BlockSummary {
        profitable: returns.iter().filter(|r| **r > 0.0).count() as f64
            / returns.len().max(1) as f64,
        median: percentile(&returns, 0.5),
        best: returns.last().copied().unwrap_or(0.0),
        worst: returns.first().copied().unwrap_or(0.0),
        worst_dates,
        blocks,
        trades,
        best_line,
        worst_line,
        median_path: median_path(&paths),
        paths,
        stride,
    }
}

/// Does `work` on every item with at most `workers` threads, calling `done` after each. Stops
/// starting more once `stop` is set; those that never ran are `None`.
pub fn pool<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    stop: &AtomicBool,
    done: &(impl Fn() + Sync),
    work: &(impl Fn(&T) -> R + Sync),
) -> Vec<Option<R>> {
    let results = Mutex::new((0..items.len()).map(|_| None).collect::<Vec<_>>());
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers.max(1) {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = work(item);
                    if let Ok(mut results) = results.lock() {
                        results[index] = Some(result);
                    }
                    done();
                }
            });
        }
    });
    results.into_inner().unwrap_or_default()
}

/// Runs a backtest on a new thread, as a chart's are: the engine keeps state per thread.
///
/// # Errors
///
/// Returns a message if it fails or stops unexpectedly.
pub fn backtest(request: BacktestRequest) -> Result<BacktestReport, String> {
    std::thread::spawn(move || persephone_engine::run_backtest(&request))
        .join()
        .map_err(|_| "A backtest stopped unexpectedly.".to_string())?
        .map_err(|e| e.to_string())
}

/// A batch of blocks to backtest, ready to go to worker threads.
pub struct BlockJob {
    /// The chart's backtest, without its bars.
    pub request: BacktestRequest,
    pub candles: Vec<Candle>,
    pub blocks: Vec<Range<usize>>,
    pub warm_up: usize,
    /// How many blocks may run at once.
    pub workers: usize,
    pub stop: Arc<AtomicBool>,
}

impl BlockJob {
    /// Backtests every block, each trading from its first bar with the bars before it to warm up
    /// on, and tells `progress` how many are done. Blocks until they're finished.
    ///
    /// # Errors
    ///
    /// Returns a message if a block's backtest fails, or the batch was stopped.
    pub fn run(self, progress: &(impl Fn(usize) + Sync)) -> Result<BlockSummary, String> {
        let longest = self.blocks.iter().map(ExactSizeIterator::len).max();
        let stride = stride_for(longest.unwrap_or(0));
        let failure = Mutex::new(None);
        let done = AtomicUsize::new(0);
        let results = pool(
            &self.blocks,
            self.workers,
            &self.stop,
            &|| progress(done.fetch_add(1, Ordering::Relaxed) + 1),
            &|block| match self.run_block(block, stride) {
                Ok(result) => Some(result),
                Err(error) => {
                    if let Ok(mut failure) = failure.lock() {
                        failure.get_or_insert(error);
                    }
                    self.stop.store(true, Ordering::Relaxed);
                    None
                }
            },
        );
        if let Some(error) = failure.into_inner().unwrap_or_default() {
            return Err(error);
        }
        if self.stop.load(Ordering::Relaxed) {
            return Err("Stopped.".into());
        }
        Ok(summarize(
            results.into_iter().flatten().flatten().collect(),
            stride,
        ))
    }

    fn run_block(&self, block: &Range<usize>, stride: usize) -> Result<BlockResult, String> {
        let warm_from = block.start.saturating_sub(self.warm_up);
        let first = self.candles[block.start].ts;
        let to = self.candles[block.end - 1].ts;
        let request = BacktestRequest {
            candles: self.candles[warm_from..block.end].to_vec(),
            trade_from: first,
            ..self.request.clone()
        };
        let capital = request.starting_balance;
        let report = backtest(request)?;
        // Its bars count from where its warm-up began; the chart's from its first bar.
        let mut trades = round_trips(&report);
        for trade in &mut trades {
            trade.entry_bar += warm_from;
            trade.exit_bar += warm_from;
        }
        let equity = report
            .equity
            .get(block.start - warm_from..)
            .unwrap_or_default();
        let end = equity.last().copied().unwrap_or(capital);
        Ok(BlockResult {
            from: first,
            to,
            return_: end / capital - 1.0,
            drawdown: deepest_fall(equity.iter().copied(), capital),
            fills: report.fills.len(),
            last_bar: block.end - 1,
            path: equity.iter().copied().step_by(stride).collect(),
            trades,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use persephone_engine::Fill;

    use super::*;

    fn fill(side: Side, price: f64, qty: f64, fee: f64) -> Fill {
        Fill {
            ts: 0,
            bar: 0,
            side,
            price,
            qty,
            fee,
        }
    }

    fn report(fills: Vec<Fill>, last_equity: f64) -> BacktestReport {
        BacktestReport {
            fills,
            equity: vec![1_000.0, last_equity],
            starting_balance: 1_000.0,
            max_drawdown: 0.0,
            fees: 0.0,
            total_orders: 0,
            total_positions: 0,
            pnl: BTreeMap::new(),
            returns: BTreeMap::new(),
            general: BTreeMap::new(),
            returns_series: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_round_trip_is_its_price_move_less_its_fees() {
        let trades = closed_trades(&report(
            vec![
                fill(Side::Buy, 100.0, 2.0, 1.0),
                fill(Side::Sell, 110.0, 2.0, 1.5),
            ],
            1_017.5,
        ));
        assert_eq!(trades.len(), 1);
        assert!(close(trades[0], 17.5), "{trades:?}");
    }

    #[test]
    fn a_short_makes_money_as_the_price_falls() {
        let trades = closed_trades(&report(
            vec![
                fill(Side::Sell, 100.0, 1.0, 0.0),
                fill(Side::Buy, 90.0, 1.0, 0.0),
            ],
            1_010.0,
        ));
        assert!(close(trades[0], 10.0), "{trades:?}");
    }

    #[test]
    fn a_reversal_closes_one_trade_and_opens_another_sharing_its_fee() {
        let trades = closed_trades(&report(
            vec![
                fill(Side::Buy, 100.0, 1.0, 0.0),
                // Half of this closes the long and half opens a short.
                fill(Side::Sell, 110.0, 2.0, 2.0),
                fill(Side::Buy, 105.0, 1.0, 0.0),
            ],
            1_013.0,
        ));
        assert_eq!(trades.len(), 2);
        assert!(close(trades[0], 9.0), "{trades:?}");
        assert!(close(trades[1], 4.0), "{trades:?}");
    }

    #[test]
    fn adding_to_a_position_averages_its_entry() {
        let trades = closed_trades(&report(
            vec![
                fill(Side::Buy, 100.0, 1.0, 0.0),
                fill(Side::Buy, 120.0, 1.0, 0.0),
                fill(Side::Sell, 130.0, 2.0, 0.0),
            ],
            1_040.0,
        ));
        assert_eq!(trades.len(), 1);
        assert!(close(trades[0], 40.0), "{trades:?}");
    }

    #[test]
    fn a_position_still_open_counts_at_the_last_close_so_it_all_adds_up() {
        let fills = vec![
            fill(Side::Buy, 100.0, 1.0, 0.5),
            fill(Side::Sell, 110.0, 1.0, 0.5),
            fill(Side::Buy, 110.0, 1.0, 0.5),
        ];
        // 9 made on the first trade; the second is 5.5 up at the last close, less its fee.
        let trades = closed_trades(&report(fills, 1_014.5));
        assert_eq!(trades.len(), 2);
        assert!(close(trades[0], 9.0), "{trades:?}");
        assert!(close(trades.iter().sum(), 14.5), "{trades:?}");
    }

    #[test]
    fn round_trips_keep_what_a_ledger_needs() {
        let mut first = fill(Side::Buy, 100.0, 1.0, 1.0);
        first.bar = 3;
        first.ts = 30;
        let mut second = fill(Side::Buy, 120.0, 1.0, 1.0);
        second.bar = 5;
        second.ts = 50;
        // Half of this closes the long, half opens a short.
        let mut reversal = fill(Side::Sell, 130.0, 4.0, 4.0);
        reversal.bar = 8;
        reversal.ts = 80;
        let trades = round_trips(&report(vec![first, second, reversal], 1_031.0));
        assert_eq!(trades.len(), 2);

        let long = &trades[0];
        assert!(long.long && !long.open);
        assert_eq!((long.entry_bar, long.entry_ts), (3, 30));
        assert_eq!((long.exit_bar, long.exit_ts), (8, 80));
        assert_eq!(long.qty, 2.0);
        assert!(close(long.entry_price, 110.0) && close(long.exit_price, 130.0));
        assert!(
            close(long.fees, 4.0) && close(long.profit, 36.0),
            "{long:?}"
        );

        // The short it reversed into starts on the same fill and is still open.
        let short = &trades[1];
        assert!(!short.long && short.open);
        assert_eq!((short.entry_bar, short.exit_bar), (8, 1));
        assert!(close(short.entry_price, 130.0));
        assert!(close(long.profit + short.profit, 31.0));
    }

    #[test]
    fn the_same_seed_gives_the_same_draws_and_another_seed_doesnt() {
        let trades: Vec<f64> = (0..30).map(|i| f64::from(i - 12) * 7.0).collect();
        let a = monte_carlo(&trades, 1_000.0, 300, 0.5, 1).unwrap();
        let b = monte_carlo(&trades, 1_000.0, 300, 0.5, 1).unwrap();
        let c = monte_carlo(&trades, 1_000.0, 300, 0.5, 2).unwrap();
        assert_eq!(a.paths, b.paths);
        assert_eq!(a.median, b.median);
        assert_ne!(a.paths, c.paths);
    }

    #[test]
    fn its_figures_are_in_order_and_its_lines_limited() {
        let trades: Vec<f64> = (0..40).map(|i| f64::from(i - 15) * 3.0).collect();
        let result = monte_carlo(&trades, 1_000.0, 1_000, 0.5, 7).unwrap();
        assert!(result.low < result.median && result.median < result.high);
        assert_eq!(result.paths.len(), MAX_PATHS);
        assert_eq!(result.paths[0].len(), trades.len() + 1);
        assert_eq!(result.paths[0][0], 1_000.0);
        assert_eq!(result.median_path.len(), trades.len() + 1);
    }

    #[test]
    fn the_best_worst_and_middle_runs_are_always_kept_to_pick_out() {
        let trades: Vec<f64> = (0..40).map(|i| f64::from(i - 15) * 3.0).collect();
        let result = monte_carlo(&trades, 1_000.0, 5_000, 0.5, 7).unwrap();
        assert_eq!(result.paths.len(), MAX_PATHS);
        assert_eq!(result.stats.len(), MAX_PATHS);
        let ends = |line: usize| result.paths[line].last().unwrap() / 1_000.0 - 1.0;
        for line in 0..MAX_PATHS {
            assert!((ends(line) - result.stats[line].return_).abs() < 1e-9);
            assert!(result.stats[line].return_ <= result.stats[result.best].return_);
            assert!(result.stats[line].return_ >= result.stats[result.worst].return_);
        }
        // The extremes of all 5,000 runs, not just of those drawn.
        assert!(result.stats[result.best].return_ >= result.high);
        assert!(result.stats[result.worst].return_ <= result.low);
        assert!((result.stats[result.middle].return_ - result.median).abs() < 0.01);
    }

    #[test]
    fn what_a_line_drew_can_be_worked_out_again_and_adds_up_to_it() {
        let trades: Vec<f64> = (0..40).map(|i| f64::from(i - 15) * 3.0).collect();
        let result = monte_carlo(&trades, 1_000.0, 5_000, 0.5, 7).unwrap();
        // Swapped-in extremes included, and whichever seed.
        for line in [0, result.best, result.worst, result.middle] {
            let draws = result
                .draws_of_run(result.run_of_line(line).unwrap())
                .unwrap();
            assert_eq!(draws.len(), trades.len());
            let made: f64 = draws.iter().map(|&i| trades[i]).sum();
            assert!((made / 1_000.0 - result.stats[line].return_).abs() < 1e-9);
        }
        // The line after the last is the trades as they came, each once.
        let real = result.draws_of_run(result.all_stats.len()).unwrap();
        assert_eq!(real, (0..trades.len()).collect::<Vec<_>>());
        assert_eq!(result.draws_of_run(result.all_stats.len() + 1), None);
        // It really does draw with replacement.
        let mut drawn = result
            .draws_of_run(result.run_of_line(result.best).unwrap())
            .unwrap();
        drawn.sort_unstable();
        drawn.dedup();
        assert!(drawn.len() < trades.len());
    }

    #[test]
    fn every_run_and_the_backtest_are_ranked_and_can_be_worked_out_again() {
        let trades: Vec<f64> = (0..40).map(|i| f64::from(i - 15) * 3.0).collect();
        let result = monte_carlo(&trades, 1_000.0, 500, 0.5, 7).unwrap();
        let rows = result.ranked_runs();
        assert_eq!(
            rows.len(),
            501,
            "all 500 runs, not just those drawn, and the backtest"
        );
        assert_eq!(rows.iter().filter(|r| r.backtest).count(), 1);
        assert_eq!(rows[0].rank, 1);
        for pair in rows.windows(2) {
            assert!(pair[0].stats.return_ >= pair[1].stats.return_);
            assert_eq!(pair[0].rank + 1, pair[1].rank);
        }
        // Whichever run, drawn as a line or not, what it drew adds up to how it went.
        for row in [rows[0], rows[250], rows[500]] {
            let draws = result.draws_of_run(row.run).unwrap();
            let made: f64 = draws.iter().map(|&i| trades[i]).sum();
            assert!((made / 1_000.0 - row.stats.return_).abs() < 1e-9);
            assert_eq!(result.stats_of_run(row.run), Some(row.stats));
        }
        assert_eq!(result.draws_of_run(502), None);
        // Sorting keeps the ranks they earned.
        let mut by_fall = rows.clone();
        sort_rows(&mut by_fall, (RunColumn::Fell, false));
        assert!(by_fall[0].stats.drawdown <= by_fall[500].stats.drawdown);
        assert_eq!(by_fall.iter().map(|r| r.rank).max(), Some(501));
        // Runs and lines map onto each other, where a run has a line.
        for line in [0, result.best, result.paths.len()] {
            let run = result.run_of_line(line).unwrap();
            assert_eq!(result.line_of_run(run), Some(line));
        }
    }

    #[test]
    fn a_runs_summary_counts_what_it_drew() {
        let profits = [10.0, -5.0, -1.0, 2.0];
        // The first trade twice, the last not at all.
        let summary = summarize_run(&[1, 2, 0, 0, 1, 1, 2], &profits);
        assert_eq!(summary.trades, 7);
        assert_eq!(summary.different, 3);
        assert_eq!(summary.winners, 2);
        assert_eq!((summary.best, summary.worst), (10.0, -5.0));
        assert!((summary.average - (10.0 + 10.0 - 5.0 - 1.0 - 5.0 - 5.0 - 1.0) / 7.0).abs() < 1e-9);
        assert_eq!(summary.losing_streak, 3);
    }

    #[test]
    fn the_real_order_of_trades_is_kept_to_compare_with() {
        let trades: Vec<f64> = [50.0, -100.0, 20.0].repeat(5);
        let result = monte_carlo(&trades, 1_000.0, 20, 0.5, 7).unwrap();
        assert_eq!(result.actual[..3], [1_000.0, 1_050.0, 950.0]);
        assert!((result.actual_stats.return_ - (-0.15)).abs() < 1e-9);
        assert!(result.actual_stats.drawdown > 0.0);
    }

    #[test]
    fn only_winners_never_ruin_and_big_losers_always_do() {
        let winners = vec![5.0; 20];
        let result = monte_carlo(&winners, 1_000.0, 200, 0.1, 3).unwrap();
        assert_eq!(result.ruin, 0.0);
        assert_eq!(result.drawdown, 0.0);
        assert!(close(result.median, 0.1));

        let losers = vec![-100.0; 20];
        let result = monte_carlo(&losers, 1_000.0, 200, 0.5, 3).unwrap();
        assert_eq!(result.ruin, 1.0);
        assert_eq!(result.drawdown, 1.0, "a fall is at most everything");
    }

    #[test]
    fn too_few_trades_say_so() {
        let error = monte_carlo(&[1.0; 9], 1_000.0, 100, 0.5, 1).unwrap_err();
        assert!(error.contains("this backtest made 9"), "{error}");
    }

    #[test]
    fn long_runs_are_thinned_to_lines_that_fit() {
        let result = monte_carlo(&vec![1.0; 5_000], 1_000.0, 10, 0.5, 1).unwrap();
        assert!(result.paths[0].len() <= MAX_POINTS + 1);
        assert_eq!(result.stride, 11);
    }

    fn candles(count: u64, every: UnixNanos) -> Vec<Candle> {
        (0..count)
            .map(|i| Candle {
                ts: i * every,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            })
            .collect()
    }

    const DAY: UnixNanos = NANOS_PER_DAY;

    #[test]
    fn blocks_stay_inside_the_span_and_are_the_length_asked_for() {
        let bars = candles(1_000, DAY);
        let blocks = pick_blocks(&bars, 200, BlockLength::Month, 300, 5).unwrap();
        assert_eq!(blocks.len(), 300);
        for block in &blocks {
            assert!(block.start >= 200, "{block:?}");
            assert!(block.end <= 1_000, "{block:?}");
            assert_eq!(block.len(), 30, "{block:?}");
        }
        assert!(blocks.iter().any(|b| b.start > 900), "later ones too");
        assert_eq!(
            blocks,
            pick_blocks(&bars, 200, BlockLength::Month, 300, 5).unwrap()
        );
        assert_ne!(
            blocks,
            pick_blocks(&bars, 200, BlockLength::Month, 300, 6).unwrap()
        );
    }

    #[test]
    fn a_span_shorter_than_a_block_says_by_how_much_and_offers_to_widen() {
        let problem = blocks_problem(&candles(13, DAY), 0, BlockLength::Month).unwrap();
        assert_eq!(
            problem.message,
            "The span covers 12 days, and 1-month blocks need 30. Widen it, or pick shorter blocks."
        );
        assert!(problem.widen);
        let short = blocks_problem(&candles(10, 60_000_000_000), 0, BlockLength::Week).unwrap();
        assert!(short.message.contains("less than a day"), "{short:?}");
        assert!(pick_blocks(&candles(13, DAY), 0, BlockLength::Month, 5, 1).is_err());
        assert!(pick_blocks(&[], 0, BlockLength::Week, 5, 1).is_err());
    }

    #[test]
    fn blocks_of_a_single_bar_are_refused_and_widening_wont_help() {
        let problem = blocks_problem(&candles(100, 30 * DAY), 0, BlockLength::Week).unwrap();
        assert!(problem.message.contains("too few bars"), "{problem:?}");
        assert!(!problem.widen);
        assert_eq!(
            pick_blocks(&candles(100, 30 * DAY), 0, BlockLength::Week, 5, 1).unwrap_err(),
            problem.message,
        );
    }

    #[test]
    fn a_widened_span_fits_the_blocks_the_history_and_the_load_limit() {
        let hour = DAY / 24;
        // Four blocks back.
        assert_eq!(
            widened_span(0, 1_000 * DAY, 30 * DAY, hour),
            Ok((880 * DAY, 1_000 * DAY))
        );
        // No further back than the history.
        assert_eq!(
            widened_span(950 * DAY, 1_000 * DAY, 30 * DAY, hour)
                .unwrap()
                .0,
            950 * DAY
        );
        assert!(widened_span(990 * DAY, 1_000 * DAY, 30 * DAY, hour).is_err());
        // No more bars than can load: minutes reach about 208 days.
        let minute = hour / 60;
        let (from, to) = widened_span(0, 1_000 * DAY, 91 * DAY, minute).unwrap();
        assert!((to - from) / minute <= MAX_RANGE_BARS);
        assert!(to - from >= 91 * DAY);
        // And blocks too long for that are refused.
        assert!(widened_span(0, 1_000 * DAY, 91 * DAY, minute / 60).is_err());
    }

    #[test]
    fn a_summary_counts_the_winners_and_finds_the_worst_dates() {
        let block = |from, return_: f64| BlockResult {
            from,
            to: from + 1,
            return_,
            drawdown: 0.0,
            fills: 1,
            last_bar: 0,
            path: vec![100.0, 100.0 * (1.0 + return_)],
            trades: Vec::new(),
        };
        let summary = summarize(
            vec![
                block(1, 0.1),
                block(2, -0.2),
                block(3, 0.3),
                block(4, 0.0),
                block(5, -0.1),
            ],
            1,
        );
        assert!(close(summary.profitable, 0.4));
        assert!(close(summary.median, 0.0));
        assert!(close(summary.best, 0.3));
        assert!(close(summary.worst, -0.2));
        assert_eq!(summary.worst_dates, (2, 3));
        assert_eq!((summary.best_line, summary.worst_line), (2, 1));
        assert_eq!(summary.blocks[1].from, 2);
        assert_eq!(summary.paths.len(), 5);
        assert!(close(summary.median_path[1], 100.0));
    }
}
