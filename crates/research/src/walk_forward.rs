//! Walk-forward: optimize on a stretch, try the best values on the unseen stretch after it, slide
//! along and repeat. What the unseen stretches come to together is the honest figure.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use persephone_core::{Candle, UnixNanos};
use persephone_engine::BacktestRequest;

use crate::{
    optimize::{Grid, OptimizeJob, Outcome, rank},
    robustness::{backtest, round_trips},
    settings::ScoreBy,
};

/// How many windows to make unless asked, and how many times longer each one's picking stretch is
/// than its unseen one.
pub const WINDOWS: usize = 5;
pub const RATIO: f64 = 3.0;

/// What one window picked and how that did on its unseen stretch.
#[derive(Clone, Debug, PartialEq)]
pub struct Picked {
    pub values: Vec<(String, f64)>,
    pub label: String,
    pub score: Option<f64>,
    pub unseen_return: f64,
    pub buy_and_hold_return: f64,
    pub fell: f64,
    pub trades: usize,
}

/// One slide along the span. `picked` is `None` if nothing could be ranked, as nothing traded.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub train_from: UnixNanos,
    pub test_from: UnixNanos,
    pub test_to: UnixNanos,
    pub picked: Option<Picked>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WalkForward {
    pub windows: Vec<Window>,
    /// The unseen stretches' returns added up, each from the same capital.
    pub unseen_return: f64,
    pub buy_and_hold_return: f64,
    pub profitable_windows: usize,
    pub deepest_fall: f64,
    pub trades: usize,
    pub train_bars: usize,
    pub test_bars: usize,
    /// The unseen stretches joined end to end, as percentages of the capital made so far, to draw,
    /// with the market's alongside. They add up to the figures above.
    pub unseen_line: Vec<f64>,
    pub market_line: Vec<f64>,
    /// Where each tested window starts in those lines, in `windows`' order.
    pub starts: Vec<(usize, usize)>,
}

/// The most points a stretch keeps for drawing.
const LINE_POINTS: usize = 300;

/// A stretch's `count` points, thinned to what a chart can use, always with the last.
fn thinned(count: usize) -> impl Iterator<Item = usize> {
    let stride = (count / LINE_POINTS).max(1);
    (0..count)
        .step_by(stride)
        .chain(std::iter::once(count.saturating_sub(1)))
}

/// A walk-forward, ready to go to worker threads.
pub struct WalkForwardJob {
    /// The chart's backtest, without its bars.
    pub request: BacktestRequest,
    pub candles: Vec<Candle>,
    /// Where the span starts in `candles`.
    pub from: usize,
    pub grid: Grid,
    pub score: ScoreBy,
    pub windows: usize,
    pub ratio: f64,
    pub warm_up: usize,
    pub runs: u32,
    pub ruin_at: f64,
    pub seed: u64,
    pub workers: usize,
    pub stop: Arc<AtomicBool>,
}

/// How long a window's picking and unseen stretches are, over `bars` bars.
///
/// # Errors
///
/// Returns a message if there are too few bars.
pub fn stretches(bars: usize, windows: usize, ratio: f64) -> Result<(usize, usize), String> {
    let test = (bars as f64 / (windows as f64 + ratio)) as usize;
    let train = (test as f64 * ratio) as usize;
    if test < 2 || train < 2 {
        return Err(format!(
            "{bars} bars is too few for {windows} windows; widen the span or ask for fewer"
        ));
    }
    Ok((train, test))
}

impl WalkForwardJob {
    /// How many backtests it comes to.
    #[must_use]
    pub fn total(&self) -> usize {
        self.windows * (self.grid.cells() + 1)
    }

    /// # Errors
    ///
    /// Returns a message if a backtest fails, or it's stopped.
    pub fn run(self, progress: &(impl Fn(usize) + Sync)) -> Result<WalkForward, String> {
        let bars = self.candles.len().saturating_sub(self.from);
        let (train, test) = stretches(bars, self.windows, self.ratio)?;
        let candles = &self.candles;
        // The last unseen stretch ends on the last bar.
        let first = candles.len() - self.windows * test - train;
        let finished = AtomicUsize::new(0);
        let mut rows = Vec::with_capacity(self.windows);
        let (mut unseen_total, mut held_total) = (0.0, 0.0);
        let (mut unseen_line, mut market_line) = (vec![0.0], vec![0.0]);
        let mut starts = Vec::with_capacity(self.windows);
        let (mut trades, mut profitable, mut deepest) = (0, 0, 0.0_f64);
        for window in 0..self.windows {
            let train_start = first + window * test;
            let train_end = train_start + train;
            let test_end = train_end + test;
            let warm_from = train_start.saturating_sub(self.warm_up);
            let mut picking = self.request.clone();
            picking.trade_from = candles[train_start].ts;
            let batch = OptimizeJob {
                request: picking,
                candles: candles[warm_from..train_end].to_vec(),
                grid: self.grid.clone(),
                score: self.score,
                split: None,
                blocks: Vec::new(),
                warm_up: self.warm_up,
                runs: self.runs,
                ruin_at: self.ruin_at,
                seed: self.seed,
                workers: self.workers,
                stop: Arc::clone(&self.stop),
            };
            let before = finished.load(Ordering::Relaxed);
            let found = batch.run(&|done| progress(before + done))?;
            finished.store(before + self.grid.cells(), Ordering::Relaxed);
            let scores: Vec<Option<f64>> = found.outcomes.iter().map(Outcome::score).collect();
            let mut row = Window {
                train_from: candles[train_start].ts,
                test_from: candles[train_end].ts,
                test_to: candles[test_end - 1].ts,
                picked: None,
            };
            let Some(&(cell, _)) = rank(&scores, self.grid.width()).first() else {
                rows.push(row);
                continue;
            };
            let mut trying = self.request.clone();
            for (index, value) in self.grid.values_of(cell) {
                trying.strategy.set_param(index, value)?;
            }
            trying.candles = candles[train_end.saturating_sub(self.warm_up)..test_end].to_vec();
            trying.trade_from = candles[train_end].ts;
            let report = backtest(trying)?;
            progress(finished.fetch_add(1, Ordering::Relaxed) + 1);
            let capital = report.starting_balance;
            let unseen = report.equity.last().copied().unwrap_or(capital) / capital - 1.0;
            let made = round_trips(&report).len();
            // From the close before the stretch's first order could fill, as a strategy's does.
            let held = candles[test_end - 1].close / candles[train_end].close - 1.0;
            let warmed = train_end - train_end.saturating_sub(self.warm_up);
            let stretch = report.equity.get(warmed..).unwrap_or_default();
            starts.push((window, unseen_line.len() - 1));
            for at in thinned(stretch.len().min(test)) {
                unseen_line.push((unseen_total + (stretch[at] / capital - 1.0)) * 100.0);
                let close = candles[train_end + at].close;
                market_line.push((held_total + (close / candles[train_end].close - 1.0)) * 100.0);
            }
            held_total += held;
            unseen_total += unseen;
            trades += made;
            profitable += usize::from(unseen > 0.0);
            let fell = report.max_drawdown.abs();
            deepest = deepest.max(fell);
            let mut values = self
                .grid
                .values_of(cell)
                .into_iter()
                .map(|(_, value)| value);
            let mut named = Vec::new();
            if let Some(value) = values.next() {
                named.push((self.grid.first.name.clone(), value));
            }
            if let (Some(second), Some(value)) = (&self.grid.second, values.next()) {
                named.push((second.name.clone(), value));
            }
            row.picked = Some(Picked {
                values: named,
                label: self.grid.label(cell),
                score: scores[cell],
                unseen_return: unseen,
                buy_and_hold_return: held,
                fell,
                trades: made,
            });
            rows.push(row);
        }
        Ok(WalkForward {
            windows: rows,
            unseen_return: unseen_total,
            buy_and_hold_return: held_total,
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
