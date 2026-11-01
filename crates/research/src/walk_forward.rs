//! Walk-forward: optimize on a stretch, try the best values on the unseen stretch after it, slide
//! along and repeat. What the unseen stretches come to together is the honest figure.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use persephone_core::{Candle, UnixNanos, signal::SignalFactory, strategy::StrategySpec};
use persephone_engine::BacktestRequest;

use crate::{
    optimize::{Grid, OptimizeJob, Outcome, rank},
    portfolio_tests::PickBy,
    robustness::{backtest, pool, round_trips},
    settings::ScoreBy,
};

/// One of the strategies a walk-forward can pick among, in place of a grid of values.
#[derive(Clone)]
pub struct StrategyChoice {
    pub label: String,
    pub strategy: StrategySpec,
    /// Makes its signal, when it isn't a built-in.
    pub plugin: Option<Arc<dyn SignalFactory>>,
}

/// What each window picks among: a grid of one strategy's values, or several strategies.
#[derive(Clone)]
pub enum Choices {
    Grid(Grid),
    /// Each picked by `PickBy` on the picking stretch.
    Strategies(Vec<StrategyChoice>, PickBy),
}

impl Choices {
    /// How many there are to try in each window.
    #[must_use]
    pub fn count(&self) -> usize {
        match self {
            Self::Grid(grid) => grid.cells(),
            Self::Strategies(list, _) => list.len(),
        }
    }
}

/// What a window picked, ready to trade on its unseen stretch.
struct Choice {
    request: BacktestRequest,
    values: Vec<(String, f64)>,
    label: String,
    score: Option<f64>,
    sharpe: Option<f64>,
    deflated: Option<f64>,
}

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
    /// Its Sharpe ratio where it was picked, and that deflated for having been the best of the grid:
    /// the chance it was more than luck there.
    pub picked_sharpe: Option<f64>,
    pub deflated_sharpe: Option<f64>,
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
    /// The Sharpe ratio of the unseen stretches' daily returns, joined end to end. Nothing was
    /// picked on these, so it needs no deflating.
    pub unseen_sharpe: Option<f64>,
    /// The market held once from the first unseen stretch's start to the last one's end, so over
    /// the same bars as `compounded`, but without the gaps and re-buying between windows that
    /// adding or compounding window by window brings.
    pub buy_and_hold_whole: f64,
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

impl WalkForward {
    /// The unseen stretches and the market's, one after another as if money stayed in, rather than each
    /// from the same capital.
    #[must_use]
    pub fn compounded(&self) -> (f64, f64) {
        let mut totals = (1.0, 1.0);
        for picked in self
            .windows
            .iter()
            .filter_map(|window| window.picked.as_ref())
        {
            totals.0 *= 1.0 + picked.unseen_return;
            totals.1 *= 1.0 + picked.buy_and_hold_return;
        }
        (totals.0 - 1.0, totals.1 - 1.0)
    }
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
    pub choices: Choices,
    pub score: ScoreBy,
    pub windows: usize,
    pub ratio: f64,
    pub warm_up: usize,
    pub runs: u32,
    pub ruin_at: f64,
    pub seed: u64,
    /// How many variants the best is picked from, if more than the grid's: those tried before
    /// count too, for its deflated Sharpe ratio. 0 counts only the grid.
    pub trials: u64,
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
        self.windows * (self.choices.count() + 1)
    }

    /// The best of the grid on a picking stretch, by the score asked for.
    fn pick_in_grid(
        &self,
        grid: &Grid,
        picking: BacktestRequest,
        candles: Vec<Candle>,
        progress: &(impl Fn(usize) + Sync),
    ) -> Result<Option<Choice>, String> {
        let batch = OptimizeJob {
            request: picking,
            candles,
            grid: grid.clone(),
            score: self.score,
            split: None,
            blocks: Vec::new(),
            warm_up: self.warm_up,
            runs: self.runs,
            ruin_at: self.ruin_at,
            seed: self.seed,
            trials: self.trials,
            workers: self.workers,
            stop: Arc::clone(&self.stop),
        };
        let found = batch.run(progress)?;
        let scores: Vec<Option<f64>> = found.outcomes.iter().map(Outcome::score).collect();
        let Some(&(cell, _)) = rank(&scores, grid.width()).first() else {
            return Ok(None);
        };
        let mut request = self.request.clone();
        for (index, value) in grid.values_of(cell) {
            request.strategy.set_param(index, value)?;
        }
        let mut values = grid.values_of(cell).into_iter().map(|(_, value)| value);
        let mut named = Vec::new();
        if let Some(value) = values.next() {
            named.push((grid.first.name.clone(), value));
        }
        if let (Some(second), Some(value)) = (&grid.second, values.next()) {
            named.push((second.name.clone(), value));
        }
        Ok(Some(Choice {
            request,
            values: named,
            label: grid.label(cell),
            score: scores[cell],
            sharpe: found.sharpes.get(cell).copied().flatten(),
            deflated: found.deflated_sharpe,
        }))
    }

    /// The best of several strategies on a picking stretch, each backtested on it alone.
    fn pick_among(
        &self,
        list: &[StrategyChoice],
        pick_by: PickBy,
        picking: &BacktestRequest,
        candles: &[Candle],
        progress: &(impl Fn(usize) + Sync),
    ) -> Result<Option<Choice>, String> {
        let times: Vec<u64> = candles.iter().map(|c| c.ts).collect();
        let warmed = times.partition_point(|&ts| ts < picking.trade_from);
        let with = |choice: &StrategyChoice| {
            let mut request = picking.clone();
            request.strategy = choice.strategy.clone();
            request.plugin = choice.plugin.clone();
            request
        };
        let done = AtomicUsize::new(0);
        let results = pool(
            list,
            self.workers,
            &self.stop,
            &|| progress(done.fetch_add(1, Ordering::Relaxed) + 1),
            &|choice| {
                let mut request = with(choice);
                request.candles = candles.to_vec();
                backtest(request).map(|report| {
                    let capital = report.starting_balance;
                    let returned = report.equity.last().copied().unwrap_or(capital) / capital - 1.0;
                    let daily = crate::figures::run_daily_returns(&report, warmed, &times);
                    (returned, crate::figures::sharpe_of_daily(&daily), daily)
                })
            },
        );
        if self.stop.load(Ordering::Relaxed) {
            return Err("Stopped.".into());
        }
        let measured = results
            .into_iter()
            .map(|result| result.transpose())
            .collect::<Result<Vec<_>, String>>()?;
        let score = |at: usize| {
            let (returned, sharpe, _) = measured[at].as_ref()?;
            match pick_by {
                PickBy::Return => Some(*returned),
                PickBy::Sharpe => *sharpe,
            }
        };
        // The best, the first listed winning a tie.
        let Some(best) = (0..list.len())
            .filter(|&at| score(at).is_some())
            .reduce(|best, at| if score(at) > score(best) { at } else { best })
        else {
            return Ok(None);
        };
        let sharpes: Vec<f64> = measured.iter().flatten().filter_map(|m| m.1).collect();
        let (_, sharpe, daily) = measured[best].as_ref().ok_or("A pick that didn't run")?;
        Ok(Some(Choice {
            request: with(&list[best]),
            values: Vec::new(),
            label: list[best].label.clone(),
            score: score(best),
            sharpe: *sharpe,
            deflated: sharpe.and_then(|sharpe| {
                crate::figures::deflated_sharpe_of(sharpe, &sharpes, self.trials, daily)
            }),
        }))
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
        let mut unseen_daily = Vec::new();
        for window in 0..self.windows {
            let train_start = first + window * test;
            let train_end = train_start + train;
            let test_end = train_end + test;
            let warm_from = train_start.saturating_sub(self.warm_up);
            let mut picking = self.request.clone();
            picking.trade_from = candles[train_start].ts;
            let before = finished.load(Ordering::Relaxed);
            let tick = |done| progress(before + done);
            let chosen = match &self.choices {
                Choices::Grid(grid) => {
                    self.pick_in_grid(grid, picking, candles[warm_from..train_end].to_vec(), &tick)?
                }
                Choices::Strategies(list, pick_by) => self.pick_among(
                    list,
                    *pick_by,
                    &picking,
                    &candles[warm_from..train_end],
                    &tick,
                )?,
            };
            finished.store(before + self.choices.count(), Ordering::Relaxed);
            let mut row = Window {
                train_from: candles[train_start].ts,
                test_from: candles[train_end].ts,
                test_to: candles[test_end - 1].ts,
                picked: None,
            };
            let Some(chosen) = chosen else {
                rows.push(row);
                continue;
            };
            let mut trying = chosen.request.clone();
            trying.candles = candles[train_end.saturating_sub(self.warm_up)..test_end].to_vec();
            trying.trade_from = candles[train_end].ts;
            let times: Vec<u64> = trying.candles.iter().map(|c| c.ts).collect();
            let warmed_up = times.partition_point(|&ts| ts < trying.trade_from);
            let report = backtest(trying)?;
            progress(finished.fetch_add(1, Ordering::Relaxed) + 1);
            unseen_daily.extend(crate::figures::run_daily_returns(
                &report, warmed_up, &times,
            ));
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
            row.picked = Some(Picked {
                values: chosen.values,
                label: chosen.label,
                score: chosen.score,
                unseen_return: unseen,
                buy_and_hold_return: held,
                fell,
                trades: made,
                picked_sharpe: chosen.sharpe,
                deflated_sharpe: chosen.deflated,
            });
            rows.push(row);
        }
        let whole_from = candles[first + train].close;
        let whole = candles.last().map_or(0.0, |last| {
            if whole_from > 0.0 {
                last.close / whole_from - 1.0
            } else {
                0.0
            }
        });
        Ok(WalkForward {
            windows: rows,
            unseen_return: unseen_total,
            buy_and_hold_return: held_total,
            buy_and_hold_whole: whole,
            unseen_sharpe: crate::figures::sharpe_of_daily(&unseen_daily),
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
