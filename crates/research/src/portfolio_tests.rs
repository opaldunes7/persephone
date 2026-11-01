//! Whether a portfolio's result is luck: the same book at other times, its returns drawn again in
//! blocks, a setting varied, and picking on one stretch to trade the unseen one after. The shifted test
//! works for one market too.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use persephone_core::{Side, UnixNanos};
use persephone_engine::{BacktestReport, BacktestRequest, PortfolioReport, PortfolioRequest};
use rand::{RngExt as _, SeedableRng as _, rngs::StdRng};

use crate::{
    figures,
    portfolio::{self, PortfolioRun, PortfolioSetup},
    positions::{Positions, RandomEntries},
    results::with_params,
    robustness::{backtest, percentile, pool},
    settings::Settings,
    walk_forward::{Picked, WalkForward, Window, stretches},
};

/// How far off its own place a shifted book must land, as a share of the span, or it's nearly itself.
const LEAST_SHIFT: f64 = 0.05;
/// The most Monte Carlo lines kept to draw.
const MOST_LINES: usize = 200;

fn offset(rng: &mut StdRng, bars: usize) -> usize {
    let least = ((bars as f64 * LEAST_SHIFT) as usize).max(1);
    if bars <= least * 2 {
        return bars / 2;
    }
    rng.random_range(least..=bars - least)
}

/// `changes` (bars in the span, in order, each with what's held from then on) moved on by `by` bars,
/// wrapping round, so a book holds the same things for as long and only the timing changes. What's in
/// force at the start is what was at the point that lands there.
fn shifted<T: Clone>(changes: &[(usize, T)], bars: usize, by: usize, flat: &T) -> Vec<(usize, T)> {
    let mut moved: Vec<(usize, T)> = changes
        .iter()
        .map(|(at, what)| ((at + by) % bars, what.clone()))
        .collect();
    moved.sort_by_key(|change| change.0);
    let in_force = moved
        .last()
        .map_or_else(|| flat.clone(), |change| change.1.clone());
    // Each before the first row in the span's own order is the one that wrapped furthest.
    let at_start = changes
        .iter()
        .rev()
        .find(|(at, _)| at + by < bars)
        .map_or(in_force, |change| change.1.clone());
    if moved.first().is_none_or(|first| first.0 != 0) {
        moved.insert(0, (0, at_start));
    }
    moved
}

fn summing_up(
    mut returns: Vec<f64>,
    mut falls: Vec<f64>,
    trades: usize,
    actual: (f64, f64),
    by_draw: Vec<(u64, f64)>,
) -> RandomEntries {
    returns.sort_by(f64::total_cmp);
    falls.sort_by(f64::total_cmp);
    let count = returns.len().max(1) as f64;
    RandomEntries {
        runs: returns.len(),
        trades,
        actual_return: actual.0,
        actual_fell: actual.1,
        share_matching_return: returns.iter().filter(|r| **r >= actual.0).count() as f64 / count,
        share_matching_fall: falls.iter().filter(|f| **f <= actual.1).count() as f64 / count,
        median: percentile(&returns, 0.5),
        low: percentile(&returns, 0.05),
        high: percentile(&returns, 0.95),
        returns,
        falls,
        by_draw,
    }
}

/// One market's run, at other times.
pub struct ShiftedJob {
    pub report: Arc<BacktestReport>,
    pub request: BacktestRequest,
    /// Where the span starts in the bars.
    pub from_index: usize,
    pub runs: usize,
    pub seed: u64,
    pub workers: usize,
    pub stop: Arc<AtomicBool>,
}

impl ShiftedJob {
    /// What it held from which bar, as moves in the span.
    fn changes(&self) -> Vec<(usize, i8)> {
        let next_open = self.request.sim.next_open;
        let mut position = 0.0;
        let mut changes: Vec<(usize, i8)> = Vec::new();
        for fill in &self.report.fills {
            position += if fill.side == Side::Buy {
                fill.qty
            } else {
                -fill.qty
            };
            let decided = if next_open {
                fill.bar.saturating_sub(1)
            } else {
                fill.bar
            };
            let held = if position.abs() < 1e-9 {
                0
            } else {
                position.signum() as i8
            };
            let at = decided.saturating_sub(self.from_index);
            match changes.last_mut() {
                Some(last) if last.0 == at => last.1 = held,
                Some(last) if last.1 == held => {}
                _ => changes.push((at, held)),
            }
        }
        changes
    }

    /// The backtest sized as the run was, with positions to come in place of its strategy, and how
    /// many bars of the span it has to move along.
    fn shell(&self) -> Result<(BacktestRequest, usize), String> {
        let mut request = self.request.clone();
        request.ticks = None;
        request.fill_bars = None;
        let bars = request
            .bars
            .closes(&request.candles)
            .len()
            .saturating_sub(self.from_index);
        if bars < 2 {
            return Err("The span's too short to move anything along.".into());
        }
        let shell = Positions::from_list(Vec::new());
        request.strategy = with_params(
            shell.spec().kind,
            Arc::new(shell.info()),
            &serde_json::json!({"params": {"Size %": request.strategy.size_fraction() * 100.0}}),
        )?;
        Ok((request, bars))
    }

    fn moved(
        &self,
        shell: &BacktestRequest,
        bars: usize,
        changes: &[(usize, i8)],
        draw: u64,
    ) -> BacktestRequest {
        let closes = shell.bars.closes(&shell.candles);
        let mut rng = StdRng::seed_from_u64(self.seed ^ (draw << 16));
        let by = offset(&mut rng, bars);
        let list = shifted(changes, bars, by, &0)
            .into_iter()
            .map(|(at, held)| (closes[self.from_index + at], held))
            .collect();
        let mut trying = shell.clone();
        trying.plugin = Some(Arc::new(Positions::from_list(list)));
        trying
    }

    /// Shift `draw` alone, as `run` backtests it, to look into its trades.
    ///
    /// # Errors
    ///
    /// Returns a message if the run never traded or the span's too short.
    pub fn placing(&self, draw: u64) -> Result<BacktestRequest, String> {
        let changes = self.changes();
        if changes.is_empty() {
            return Err("That run made no trades, so there's no timing to move.".into());
        }
        let (shell, bars) = self.shell()?;
        Ok(self.moved(&shell, bars, &changes, draw))
    }

    /// # Errors
    ///
    /// Returns a message if the run never traded, a backtest fails, or it's stopped before any finish.
    pub fn run(self, progress: &(impl Fn(usize) + Sync)) -> Result<RandomEntries, String> {
        let changes = self.changes();
        if changes.is_empty() {
            return Err("That run made no trades, so there's no timing to move.".into());
        }
        let (shell, bars) = self.shell()?;
        let draws: Vec<u64> = (0..self.runs as u64).collect();
        let done = AtomicUsize::new(0);
        let results = pool(
            &draws,
            self.workers,
            &self.stop,
            &|| progress(done.fetch_add(1, Ordering::Relaxed) + 1),
            &|&draw| {
                backtest(self.moved(&shell, bars, &changes, draw)).map(|run| {
                    let capital = run.starting_balance;
                    let last = run.equity.last().copied().unwrap_or(capital);
                    (last / capital - 1.0, run.max_drawdown.abs())
                })
            },
        );
        let (mut returns, mut falls, mut by_draw) = (Vec::new(), Vec::new(), Vec::new());
        for (draw, result) in draws.iter().zip(results) {
            let Some(result) = result else { continue };
            let (returned, fell) = result?;
            returns.push(returned);
            falls.push(fell);
            by_draw.push((*draw, returned));
        }
        if returns.is_empty() {
            return Err("Stopped.".into());
        }
        let capital = self.report.starting_balance;
        let actual = self.report.equity.last().copied().unwrap_or(capital) / capital - 1.0;
        Ok(summing_up(
            returns,
            falls,
            changes.len(),
            (actual, self.report.max_drawdown.abs()),
            by_draw,
        ))
    }
}

/// A portfolio's book as weights at each moment it changed, in the span, however it was decided.
fn book_of(report: &PortfolioReport, from_index: usize, next_open: bool) -> Vec<(usize, Vec<f64>)> {
    let mut moments: Vec<usize> = report
        .legs
        .iter()
        .flat_map(|leg| leg.fills.iter().map(|fill| fill.bar))
        .collect();
    moments.sort_unstable();
    moments.dedup();
    moments
        .into_iter()
        .filter_map(|bar| {
            let equity = *report.equity.get(bar)?;
            if equity <= 0.0 {
                return None;
            }
            let decided = if next_open {
                bar.saturating_sub(1)
            } else {
                bar
            };
            let weights = report
                .legs
                .iter()
                .map(|leg| leg.values.get(bar).copied().unwrap_or(0.0) / equity)
                .collect();
            Some((decided.saturating_sub(from_index), weights))
        })
        .collect()
}

/// A portfolio at other times: its book as a table of weights, moved along. The table run with no
/// move is what the others are held against, as sizing from weights isn't quite how a strategy sizes.
pub struct ShiftedPortfolioJob {
    pub report: Arc<PortfolioReport>,
    pub request: PortfolioRequest,
    pub from_index: usize,
    pub runs: usize,
    pub seed: u64,
    pub workers: usize,
    pub stop: Arc<AtomicBool>,
}

impl ShiftedPortfolioJob {
    /// # Errors
    ///
    /// Returns a message if the run never traded, a run fails, or it's stopped before any finish.
    pub fn run(self, progress: &(impl Fn(usize) + Sync)) -> Result<RandomEntries, String> {
        let report = &self.report;
        // A table is already the book, row for row: anything else is worked out from what it did.
        let given = self.request.weights.as_ref().map(|table| {
            let mut rows: Vec<(usize, Vec<f64>)> = Vec::with_capacity(table.len());
            for (when, weights) in table.iter() {
                let at = report
                    .times
                    .partition_point(|time| time < when)
                    .saturating_sub(self.from_index);
                match rows.last_mut() {
                    Some(last) if last.0 == at => last.1.clone_from(weights),
                    _ => rows.push((at, weights.clone())),
                }
            }
            rows
        });
        let is_table = given.is_some();
        let book =
            given.unwrap_or_else(|| book_of(report, self.from_index, self.request.sim.next_open));
        if book.is_empty() {
            return Err("That run made no trades, so there's no timing to move.".into());
        }
        let bars = report.times.len().saturating_sub(self.from_index);
        if bars < 2 {
            return Err("The span's too short to move anything along.".into());
        }
        let times = &report.times;
        let flat = vec![0.0; report.legs.len()];
        let with_table = |moved: Vec<(usize, Vec<f64>)>| {
            let mut trying = self.request.clone();
            for leg in &mut trying.legs {
                leg.strategy = None;
                leg.plugin = None;
            }
            if !is_table {
                trying.rebalance = persephone_engine::Rebalance::default();
            }
            trying.weights = Some(Arc::new(
                moved
                    .into_iter()
                    .map(|(at, weights)| (times[self.from_index + at], weights))
                    .collect(),
            ));
            trying
        };
        let figures_of = |request: PortfolioRequest| {
            portfolio::run(request, false)
                .map(|run| (run.report.net_return(), run.report.max_drawdown.abs()))
        };
        let actual = if is_table {
            (report.net_return(), report.max_drawdown.abs())
        } else {
            figures_of(with_table(book.clone()))?
        };
        let draws: Vec<u64> = (0..self.runs as u64).collect();
        let done = AtomicUsize::new(0);
        let results = pool(
            &draws,
            self.workers,
            &self.stop,
            &|| progress(done.fetch_add(1, Ordering::Relaxed) + 1),
            &|&draw| {
                let mut rng = StdRng::seed_from_u64(self.seed ^ (draw << 16));
                let by = offset(&mut rng, bars);
                figures_of(with_table(shifted(&book, bars, by, &flat)))
            },
        );
        let (mut returns, mut falls) = (Vec::new(), Vec::new());
        for result in results.into_iter().flatten() {
            let (returned, fell) = result?;
            returns.push(returned);
            falls.push(fell);
        }
        if returns.is_empty() {
            return Err("Stopped.".into());
        }
        // Each draw here is a weight table, not a run to follow on one chart.
        Ok(summing_up(returns, falls, book.len(), actual, Vec::new()))
    }
}

/// What the account's returns come to, drawn again in blocks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PortfolioMonteCarlo {
    pub runs: usize,
    pub block_bars: usize,
    pub actual_return: f64,
    pub actual_fell: f64,
    /// Sorted, to draw and read shares from.
    pub returns: Vec<f64>,
    pub falls: Vec<f64>,
    pub median: f64,
    pub low: f64,
    pub high: f64,
    /// The share that fell past the ruin level, and that lost money.
    pub ruin_share: f64,
    pub losing_share: f64,
    /// A few of them as lines of the account, thinned to what can be drawn, and the middle one of all.
    pub lines: Vec<Vec<f64>>,
    pub median_line: Vec<f64>,
    /// Where 19 in 20 of them were at each point, to draw as a band.
    pub low_line: Vec<f64>,
    pub high_line: Vec<f64>,
    pub actual_line: Vec<f64>,
    pub stride: usize,
}

/// Draws the account's returns along the timeline in blocks, with replacement, each run as long as the
/// span. Blocks of moments keep how the legs moved together and how returns follow each other, which
/// shuffling trades by leg would lose. Fills and costs aren't run again.
///
/// # Errors
///
/// Returns a message if the span's too short.
pub fn monte_carlo(
    report: &PortfolioReport,
    from_index: usize,
    runs: usize,
    block_bars: usize,
    ruin_at: f64,
    seed: u64,
) -> Result<PortfolioMonteCarlo, String> {
    let equity = report.equity.get(from_index..).unwrap_or(&[]);
    let capital = report.starting_balance;
    let returns: Vec<f64> = std::iter::once(capital)
        .chain(equity.iter().copied())
        .collect::<Vec<_>>()
        .windows(2)
        .map(|pair| {
            if pair[0] > 0.0 {
                pair[1] / pair[0] - 1.0
            } else {
                0.0
            }
        })
        .collect();
    let count = returns.len();
    if count < 2 {
        return Err("The span's too short to draw from.".into());
    }
    let block = block_bars.clamp(1, count);
    let stride = count.div_ceil(300).max(1);
    let thin = |line: &[f64]| -> Vec<f64> { line.iter().step_by(stride).copied().collect() };
    let mut rng = StdRng::seed_from_u64(seed);
    let mut results: Vec<(f64, f64)> = Vec::with_capacity(runs);
    let mut all_lines: Vec<(f64, Vec<f64>)> = Vec::new();
    for _ in 0..runs {
        let mut value = capital;
        let (mut peak, mut fell) = (capital, 0.0_f64);
        let mut line = Vec::with_capacity(count);
        while line.len() < count {
            let start = rng.random_range(0..=count - block);
            for r in returns.iter().skip(start).take(block) {
                if line.len() == count {
                    break;
                }
                value = (value * (1.0 + r)).max(0.0);
                peak = peak.max(value);
                if peak > 0.0 {
                    fell = fell.max(1.0 - value / peak);
                }
                line.push(value);
            }
        }
        let returned = value / capital - 1.0;
        results.push((returned, fell));
        all_lines.push((returned, thin(&line)));
    }
    let mut sorted_returns: Vec<f64> = results.iter().map(|r| r.0).collect();
    let mut falls: Vec<f64> = results.iter().map(|r| r.1).collect();
    sorted_returns.sort_by(f64::total_cmp);
    falls.sort_by(f64::total_cmp);
    let median = percentile(&sorted_returns, 0.5);
    all_lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    let median_line = all_lines
        .get(all_lines.len() / 2)
        .map_or_else(Vec::new, |line| line.1.clone());
    let points = all_lines.first().map_or(0, |line| line.1.len());
    let (mut low_line, mut high_line) = (Vec::with_capacity(points), Vec::with_capacity(points));
    for point in 0..points {
        let mut here: Vec<f64> = all_lines.iter().map(|line| line.1[point]).collect();
        here.sort_by(f64::total_cmp);
        low_line.push(percentile(&here, 0.05));
        high_line.push(percentile(&here, 0.95));
    }
    let keep = all_lines.len().div_ceil(MOST_LINES).max(1);
    let lines = all_lines
        .into_iter()
        .step_by(keep)
        .map(|line| line.1)
        .collect();
    let total = runs.max(1) as f64;
    let last = equity.last().copied().unwrap_or(capital);
    Ok(PortfolioMonteCarlo {
        runs,
        block_bars: block,
        actual_return: last / capital - 1.0,
        actual_fell: report.max_drawdown.abs(),
        median,
        low: percentile(&sorted_returns, 0.05),
        high: percentile(&sorted_returns, 0.95),
        ruin_share: falls.iter().filter(|fall| **fall >= ruin_at).count() as f64 / total,
        losing_share: sorted_returns.iter().filter(|r| **r < 0.0).count() as f64 / total,
        returns: sorted_returns,
        falls,
        lines,
        median_line,
        low_line,
        high_line,
        actual_line: thin(equity),
        stride,
    })
}

/// A setup to try, among others.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub label: String,
    pub values: Vec<(String, f64)>,
    pub request: PortfolioRequest,
}

/// What can be varied from the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vary {
    /// One leg's weight.
    Weight(usize),
    /// Every leg's weight, scaled together.
    Scale,
    RebalanceEvery,
    RebalanceDrift,
    SlippagePercent,
    Leverage,
}

impl Vary {
    #[must_use]
    pub fn name(self, setup: &PortfolioSetup) -> String {
        match self {
            Self::Weight(leg) => setup.legs.get(leg).map_or_else(
                || "A weight".to_owned(),
                |leg| format!("{} weight", leg.symbol),
            ),
            Self::Scale => "All weights ×".to_owned(),
            Self::RebalanceEvery => "Rebalance every".to_owned(),
            Self::RebalanceDrift => "Rebalance drift".to_owned(),
            Self::SlippagePercent => "Slippage %".to_owned(),
            Self::Leverage => "Leverage".to_owned(),
        }
    }

    /// `setup` and `settings` with this set to `value`.
    pub fn apply(self, value: f64, setup: &mut PortfolioSetup, settings: &mut Settings) {
        match self {
            Self::Weight(leg) => {
                if let Some(leg) = setup.legs.get_mut(leg) {
                    leg.weight = value;
                }
            }
            Self::Scale => {
                for leg in &mut setup.legs {
                    leg.weight *= value;
                }
            }
            Self::RebalanceEvery => setup.rebalance.every_bars = Some(value.max(1.0) as u32),
            Self::RebalanceDrift => setup.rebalance.drift_percent = Some(value),
            Self::SlippagePercent => {
                settings.slippage_percent.on = true;
                settings.slippage_percent.value = value;
            }
            Self::Leverage => {
                settings.leverage.on = true;
                settings.leverage.value = value;
            }
        }
    }
}

/// The values from `min` to `max` by `step`, ends included.
///
/// # Errors
///
/// Returns a message if they make no sense or there are too many.
pub fn range_of(min: f64, max: f64, step: f64) -> Result<Vec<f64>, String> {
    const MOST: usize = 200;
    if step.is_nan() || step <= 0.0 || max.is_nan() || min.is_nan() || max < min {
        return Err("Max has to be at least Min, and Step above 0.".into());
    }
    let count = ((max - min) / step + 1e-9).floor() as usize + 1;
    if count > MOST {
        return Err(format!(
            "That's {count} values, more than the {MOST} allowed. Use a larger Step."
        ));
    }
    Ok((0..count).map(|at| min + at as f64 * step).collect())
}

/// One of a sweep's setups, and how it did.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepRow {
    pub label: String,
    pub net_return: f64,
    pub sharpe: Option<f64>,
    pub fell: f64,
    pub turnover: f64,
    pub peak_margin_use: f64,
    pub liquidated: bool,
}

/// Runs each candidate, best first.
///
/// # Errors
///
/// Returns a message if one fails, or it's stopped before any finish.
pub fn sweep(
    candidates: &[Candidate],
    workers: usize,
    stop: &AtomicBool,
    progress: &(impl Fn(usize) + Sync),
) -> Result<Vec<SweepRow>, String> {
    let done = AtomicUsize::new(0);
    let results = pool(
        candidates,
        workers,
        stop,
        &|| progress(done.fetch_add(1, Ordering::Relaxed) + 1),
        &|candidate| {
            portfolio::run(candidate.request.clone(), false).map(|run| SweepRow {
                label: candidate.label.clone(),
                net_return: run.report.net_return(),
                sharpe: run.figures.sharpe,
                fell: run.report.max_drawdown.abs(),
                turnover: run.report.turnover,
                peak_margin_use: run.report.peak_margin_use,
                liquidated: run.report.liquidated_at.is_some(),
            })
        },
    );
    let mut rows = Vec::new();
    for result in results.into_iter().flatten() {
        rows.push(result?);
    }
    if rows.is_empty() {
        return Err("Stopped.".into());
    }
    rows.sort_by(|a, b| b.net_return.total_cmp(&a.net_return));
    Ok(rows)
}

/// What a walk-forward picks by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickBy {
    #[default]
    Return,
    Sharpe,
}

/// Each window picks the best candidate on its stretch, and trades it on the unseen one after.
pub struct WalkJob {
    pub candidates: Vec<Candidate>,
    pub windows: usize,
    pub ratio: f64,
    pub warm_up: usize,
    pub pick_by: PickBy,
    pub workers: usize,
    pub stop: Arc<AtomicBool>,
}

impl WalkJob {
    /// How many runs it comes to.
    #[must_use]
    pub fn total(&self) -> usize {
        self.windows * (self.candidates.len() + 1)
    }

    /// A candidate over the bars opening in `[from, to)`, with some before them to warm up on.
    fn over(&self, request: &PortfolioRequest, from: UnixNanos, to: UnixNanos) -> PortfolioRequest {
        over(request, from, to, self.warm_up)
    }

    /// # Errors
    ///
    /// Returns a message if the span's too short, a run fails, or it's stopped.
    pub fn run(self, progress: &(impl Fn(usize) + Sync)) -> Result<WalkForward, String> {
        let first = self
            .candidates
            .first()
            .ok_or("Give it at least one setup to pick from.")?;
        let mut opens: Vec<UnixNanos> = first
            .request
            .legs
            .iter()
            .flat_map(|leg| leg.candles.iter().map(|c| c.ts))
            .collect();
        opens.sort_unstable();
        opens.dedup();
        let from = opens.partition_point(|open| *open < first.request.trade_from);
        let (train, test) = stretches(opens.len().saturating_sub(from), self.windows, self.ratio)?;
        let done = AtomicUsize::new(0);
        let tick = || progress(done.fetch_add(1, Ordering::Relaxed) + 1);
        let time_at = |at: usize| opens.get(at).copied().unwrap_or(UnixNanos::MAX);
        let mut windows = Vec::with_capacity(self.windows);
        let (mut unseen_line, mut market_line, mut starts) = (vec![0.0], vec![0.0], Vec::new());
        let mut unseen_daily = Vec::new();
        let (mut unseen_return, mut market_return, mut deepest, mut profitable, mut trades) =
            (0.0, 0.0, 0.0_f64, 0, 0);
        for window in 0..self.windows {
            let train_at = from + window * test;
            let test_at = train_at + train;
            let last_window = window + 1 == self.windows;
            let test_to = if last_window {
                opens.len()
            } else {
                test_at + test
            };
            let (train_from, test_from, test_until) =
                (time_at(train_at), time_at(test_at), time_at(test_to));
            let tries = pool(
                &self.candidates,
                self.workers,
                &self.stop,
                &tick,
                &|candidate| {
                    let run = portfolio::run(
                        self.over(&candidate.request, train_from, test_from),
                        false,
                    )?;
                    Ok::<_, String>((
                        score_of(&run, self.pick_by),
                        run.figures.sharpe,
                        portfolio_daily(&run),
                    ))
                },
            );
            let mut best: Option<(usize, f64)> = None;
            let mut sharpes = Vec::with_capacity(tries.len());
            let mut dailies = Vec::with_capacity(tries.len());
            for (at, tried) in tries.into_iter().enumerate() {
                if self.stop.load(Ordering::Relaxed) {
                    return Err("Stopped.".into());
                }
                let Some((score, sharpe, daily)) = tried.transpose()? else {
                    sharpes.push(None);
                    dailies.push(Vec::new());
                    continue;
                };
                sharpes.push(sharpe);
                dailies.push(daily);
                let Some(score) = score else {
                    continue;
                };
                if best.is_none_or(|(_, top)| score > top) {
                    best = Some((at, score));
                }
            }
            let tried: Vec<f64> = sharpes.iter().flatten().copied().collect();
            let picked = match best {
                Some((at, score)) => {
                    let candidate = &self.candidates[at];
                    let run = portfolio::run(
                        self.over(&candidate.request, test_from, test_until),
                        false,
                    )?;
                    tick();
                    let report = &run.report;
                    let fills: usize = report.legs.iter().map(|leg| leg.fills.len()).sum();
                    starts.push((windows.len(), unseen_line.len() - 1));
                    let (made_before, market_before) = (
                        unseen_line.last().copied().unwrap_or(0.0),
                        market_line.last().copied().unwrap_or(0.0),
                    );
                    let capital = report.starting_balance;
                    for (equity, held) in report
                        .equity
                        .iter()
                        .zip(&run.figures.benchmark)
                        .skip(run.from_index)
                    {
                        unseen_line.push(made_before + (equity / capital - 1.0) * 100.0);
                        market_line.push(market_before + (held / capital - 1.0) * 100.0);
                    }
                    unseen_daily.extend(portfolio_daily(&run));
                    let returned = report.net_return();
                    let market = run.figures.benchmark_return;
                    unseen_return += returned;
                    market_return += market;
                    deepest = deepest.max(report.max_drawdown.abs());
                    trades += fills;
                    if returned > 0.0 {
                        profitable += 1;
                    }
                    Some(Picked {
                        values: candidate.values.clone(),
                        label: candidate.label.clone(),
                        score: Some(score),
                        unseen_return: returned,
                        buy_and_hold_return: market,
                        fell: report.max_drawdown.abs(),
                        trades: fills,
                        picked_sharpe: sharpes[at],
                        deflated_sharpe: sharpes[at].and_then(|sharpe| {
                            figures::deflated_sharpe_of(sharpe, &tried, &dailies[at])
                        }),
                    })
                }
                None => None,
            };
            windows.push(Window {
                train_from,
                test_from,
                test_to: test_until.min(opens.last().copied().unwrap_or(test_until)),
                picked,
            });
        }
        // Equal shares of the markets, held once over every unseen stretch together.
        let whole_from = time_at(from + train);
        let held: Vec<f64> = first
            .request
            .legs
            .iter()
            .filter_map(|leg| {
                let start = leg.candles.iter().find(|c| c.ts >= whole_from)?;
                let end = leg.candles.last()?;
                (start.close > 0.0).then(|| end.close / start.close - 1.0)
            })
            .collect();
        let whole = if held.is_empty() {
            0.0
        } else {
            held.iter().sum::<f64>() / held.len() as f64
        };
        Ok(WalkForward {
            windows,
            unseen_return,
            buy_and_hold_return: market_return,
            buy_and_hold_whole: whole,
            unseen_sharpe: figures::sharpe_of_daily(&unseen_daily),
            profitable_windows: profitable,
            deepest_fall: deepest,
            trades,
            train_bars: train,
            test_bars: test,
            unseen_line,
            market_line,
            starts,
        })
    }
}

/// `request` trading only from `from` until `to`, with `warm_up` bars before for its strategies, as
/// a walk-forward runs each stretch.
#[must_use]
pub fn over(
    request: &PortfolioRequest,
    from: UnixNanos,
    to: UnixNanos,
    warm_up: usize,
) -> PortfolioRequest {
    let mut cut = request.clone();
    cut.trade_from = from;
    for leg in &mut cut.legs {
        let first = leg.candles.partition_point(|c| c.ts < from);
        let last = leg.candles.partition_point(|c| c.ts < to);
        let start = first.saturating_sub(warm_up);
        leg.candles = leg.candles[start..last].to_vec();
    }
    cut.legs.retain(|leg| !leg.candles.is_empty());
    cut
}

/// A portfolio run's daily returns from the span's start, as its Sharpe ratio is worked out.
fn portfolio_daily(run: &PortfolioRun) -> Vec<f64> {
    figures::daily_returns(
        run.report.starting_balance,
        run.opens
            .iter()
            .zip(&run.report.equity)
            .skip(run.from_index)
            .map(|(&time, &equity)| (time, equity)),
    )
}

/// How a candidate did on a stretch. One that stayed flat made nothing, which beats losing, so it's
/// there to be picked like any other.
fn score_of(run: &PortfolioRun, pick_by: PickBy) -> Option<f64> {
    match pick_by {
        PickBy::Return => Some(run.report.net_return()),
        PickBy::Sharpe => Some(run.figures.sharpe.unwrap_or(0.0)),
    }
}

#[cfg(test)]
mod tests {
    use persephone_core::{Candle, InstrumentSpec, Timeframe};
    use persephone_engine::{FeeRates, PortfolioLeg, Rebalance, SimSettings, TradingRules};

    use super::*;

    const HOUR: u64 = 3_600_000_000_000;

    fn leg(symbol: &str, closes: &[f64], weight: f64) -> PortfolioLeg {
        PortfolioLeg {
            instrument: InstrumentSpec {
                symbol: symbol.into(),
                venue: "BINANCE".into(),
                base: symbol.into(),
                quote: "USDT".into(),
                price_precision: 2,
                size_precision: 4,
            },
            candles: closes
                .iter()
                .enumerate()
                .map(|(at, &close)| Candle {
                    ts: at as u64 * HOUR,
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 100_000.0,
                })
                .collect(),
            strategy: None,
            plugin: None,
            weight,
            fees: FeeRates::ZERO,
            funding: None,
        }
    }

    fn request(legs: Vec<PortfolioLeg>) -> PortfolioRequest {
        PortfolioRequest {
            legs,
            bars: Timeframe::H1.into(),
            weights: None,
            rebalance: Rebalance::default(),
            starting_balance: 100_000.0,
            sim: SimSettings::default(),
            rules: TradingRules::default(),
            trade_from: 0,
            maintenance_percent: 0.5,
        }
    }

    #[test]
    fn a_shift_moves_every_change_wrapping_round_and_keeps_what_is_in_force() {
        let changes = [(2, 1_i8), (5, -1), (8, 0)];
        // By 3 of 10: 5, 8 and 1, so 1 comes first, and at the start is what was in force at 7, -1.
        let moved = shifted(&changes, 10, 3, &0);
        assert_eq!(moved, vec![(0, -1), (1, 0), (5, 1), (8, -1)]);
        // By 0 it's as it was, flat until the first.
        assert_eq!(shifted(&changes, 10, 0, &0)[0], (0, 0));
        assert_eq!(shifted(&changes, 10, 0, &0)[1], (2, 1));
    }

    #[test]
    fn a_book_with_hindsight_beats_itself_shifted() {
        // Long only through the rises of a wave: nothing else could do this well.
        let closes: Vec<f64> = (0..400)
            .map(|at| 100.0 + 30.0 * (f64::from(at) / 20.0).sin())
            .map(|close| (close * 100.0).round() / 100.0)
            .collect();
        let mut table = Vec::new();
        for at in 0..399 {
            let rising = closes[at + 1] > closes[at];
            table.push(((at as u64 + 1) * HOUR, vec![if rising { 0.9 } else { 0.0 }]));
        }
        let mut asked = request(vec![leg("AAA", &closes, 0.0)]);
        asked.weights = Some(Arc::new(table));
        let run = portfolio::run(asked.clone(), false).unwrap();
        let found = ShiftedPortfolioJob {
            report: Arc::new(run.report),
            request: asked,
            from_index: 0,
            runs: 30,
            seed: 1,
            workers: 4,
            stop: Arc::default(),
        }
        .run(&|_| {})
        .unwrap();
        assert!(found.actual_return > 0.5, "{}", found.actual_return);
        assert!(found.share_matching_return < 0.05, "{found:?}");
        assert!(found.median < found.actual_return);
    }

    #[test]
    fn a_book_with_no_foresight_sits_among_its_shifted_selves() {
        let mut rng = StdRng::seed_from_u64(7);
        let mut close = 100.0_f64;
        let closes: Vec<f64> = (0..600)
            .map(|_| {
                close *= 1.0 + rng.random_range(-0.01..0.01);
                (close * 100.0).round() / 100.0
            })
            .collect();
        let table: Vec<(u64, Vec<f64>)> = (0..30)
            .map(|at| {
                (
                    (at * 20 + 1) * HOUR,
                    vec![if rng.random_bool(0.5) { 0.8 } else { -0.8 }],
                )
            })
            .collect();
        let mut asked = request(vec![leg("AAA", &closes, 0.0)]);
        asked.weights = Some(Arc::new(table));
        let run = portfolio::run(asked.clone(), false).unwrap();
        let found = ShiftedPortfolioJob {
            report: Arc::new(run.report),
            request: asked,
            from_index: 0,
            runs: 60,
            seed: 2,
            workers: 4,
            stop: Arc::default(),
        }
        .run(&|_| {})
        .unwrap();
        assert!(
            (0.02..0.98).contains(&found.share_matching_return),
            "{}",
            found.share_matching_return
        );
    }

    #[test]
    fn drawing_blocks_is_repeatable_and_a_steady_climb_stays_a_climb() {
        let report = PortfolioReport {
            starting_balance: 100.0,
            equity: (1..=100).map(|at| 100.0 * 1.01_f64.powi(at)).collect(),
            times: (1..=100).collect(),
            ..PortfolioReport::default()
        };
        let a = monte_carlo(&report, 0, 50, 10, 0.5, 3).unwrap();
        assert_eq!(a, monte_carlo(&report, 0, 50, 10, 0.5, 3).unwrap());
        // All returns are the same, so however they're drawn it comes to the same.
        assert!((a.median - a.actual_return).abs() < 1e-9);
        assert_eq!(a.falls.last(), Some(&0.0));
        assert_eq!(a.ruin_share, 0.0);
        assert_eq!(a.losing_share, 0.0);
        assert!(a.lines.len() <= MOST_LINES);

        // With a crash in one block only, some draws miss it, and some hit it more than once.
        let mut equity = vec![100.0; 50];
        equity.extend(vec![50.0; 50]);
        let crash = PortfolioReport {
            starting_balance: 100.0,
            times: (1..=100).collect(),
            equity,
            ..PortfolioReport::default()
        };
        let drawn = monte_carlo(&crash, 0, 400, 10, 0.4, 4).unwrap();
        assert!(drawn.returns[0] < -0.6, "{}", drawn.returns[0]);
        assert!(drawn.returns.last().is_some_and(|best| *best == 0.0));
        assert!(
            drawn.ruin_share > 0.1 && drawn.ruin_share < 0.95,
            "{}",
            drawn.ruin_share
        );
    }

    #[test]
    fn ranges_include_their_ends_and_refuse_nonsense() {
        assert_eq!(range_of(0.1, 0.3, 0.1).unwrap().len(), 3);
        assert!(range_of(1.0, 0.0, 0.1).is_err());
        assert!(range_of(0.0, 1.0, 0.0).is_err());
        assert!(
            range_of(0.0, 1000.0, 1.0)
                .unwrap_err()
                .contains("more than")
        );
    }

    #[test]
    fn a_walk_picks_on_each_stretch_and_is_judged_on_the_next() {
        // Up for the first half and down for the second: long does well, then short.
        let mut closes: Vec<f64> = (0..200).map(|at| 100.0 + f64::from(at)).collect();
        closes.extend((0..200).map(|at| 300.0 - f64::from(at)));
        let candidate = |weight: f64, label: &str| Candidate {
            label: label.to_owned(),
            values: vec![("Weight".to_owned(), weight)],
            request: request(vec![leg("AAA", &closes, weight)]),
        };
        let found = WalkJob {
            candidates: vec![candidate(0.5, "long"), candidate(-0.5, "short")],
            windows: 3,
            ratio: 1.0,
            warm_up: 0,
            pick_by: PickBy::Return,
            workers: 2,
            stop: Arc::default(),
        }
        .run(&|_| {})
        .unwrap();
        // A hundred bars each: picked on the first hundred, long's the best for the second.
        assert_eq!(found.windows.len(), 3);
        let label = |at: usize| found.windows[at].picked.as_ref().unwrap().label.as_str();
        assert_eq!(label(0), "long");
        assert!(found.windows[0].picked.as_ref().unwrap().unseen_return > 0.0);
        assert_eq!(found.train_bars, found.test_bars);
        // The last picks on what was falling, and is judged on the same fall.
        assert_eq!(label(2), "short");
        assert!(found.windows[2].picked.as_ref().unwrap().unseen_return > 0.0);
        let sum: f64 = found
            .windows
            .iter()
            .map(|window| window.picked.as_ref().unwrap().unseen_return)
            .sum();
        assert!((sum - found.unseen_return).abs() < 1e-9);
        assert!((found.unseen_line.last().unwrap() - sum * 100.0).abs() < 1e-6);
    }

    #[test]
    fn staying_flat_can_be_picked_and_beats_losing() {
        let closes: Vec<f64> = (0..300).map(|at| 400.0 - f64::from(at)).collect();
        let flat = {
            let mut asked = request(vec![leg("AAA", &closes, 0.0)]);
            asked.weights = Some(Arc::new(vec![(0, vec![0.0])]));
            Candidate {
                label: "flat".to_owned(),
                values: Vec::new(),
                request: asked,
            }
        };
        let long = Candidate {
            label: "long".to_owned(),
            values: Vec::new(),
            request: request(vec![leg("AAA", &closes, 0.5)]),
        };
        let found = WalkJob {
            candidates: vec![long, flat],
            windows: 2,
            ratio: 1.0,
            warm_up: 0,
            pick_by: PickBy::Return,
            workers: 2,
            stop: Arc::default(),
        }
        .run(&|_| {})
        .unwrap();
        for window in &found.windows {
            let picked = window.picked.as_ref().unwrap();
            assert_eq!(picked.label, "flat");
            assert_eq!(picked.unseen_return, 0.0);
        }
    }

    #[test]
    fn a_table_is_its_own_book_when_shifted() {
        let closes: Vec<f64> = (0..200)
            .map(|at| (10_000.0 + 2_000.0 * (f64::from(at) / 15.0).sin()).round() / 100.0)
            .collect();
        let mut asked = request(vec![leg("AAA", &closes, 0.0)]);
        asked.weights = Some(Arc::new(
            (0..10)
                .map(|at| {
                    (
                        at * 20 * HOUR + 1,
                        vec![if at % 2 == 0 { 0.5 } else { -0.5 }],
                    )
                })
                .collect(),
        ));
        let run = portfolio::run(asked.clone(), false).unwrap();
        let (returned, fell) = (run.report.net_return(), run.report.max_drawdown.abs());
        let found = ShiftedPortfolioJob {
            report: Arc::new(run.report),
            request: asked,
            from_index: 0,
            runs: 4,
            seed: 1,
            workers: 2,
            stop: Arc::default(),
        }
        .run(&|_| {})
        .unwrap();
        assert_eq!(found.actual_return, returned);
        assert_eq!(found.actual_fell, fell);
        assert_eq!(found.trades, 10);
    }

    #[test]
    fn a_sweep_ranks_its_setups_best_first() {
        let closes: Vec<f64> = (0..50).map(|at| 100.0 + f64::from(at)).collect();
        let candidate = |weight: f64| Candidate {
            label: format!("{weight}"),
            values: Vec::new(),
            request: request(vec![leg("AAA", &closes, weight)]),
        };
        let rows = sweep(
            &[candidate(0.2), candidate(0.8), candidate(0.5)],
            2,
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, vec!["0.8", "0.5", "0.2"]);
    }
}
