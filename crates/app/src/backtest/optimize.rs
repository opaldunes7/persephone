//! The Optimize test: backtests across a range of one or two of a strategy's parameters, scored
//! by a plain backtest, by Monte Carlo or by random blocks.
//!
//! The highest score in a grid of tries is partly luck, and more so the more tries there are. So
//! values are ranked with their neighbours, as a good value in a good area is more likely to be
//! real than a lone spike, and the span's last part can be kept back to check the best on bars
//! they weren't picked on.

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use persephone_core::{Candle, indicator::Param};
use persephone_engine::{BacktestReport, BacktestRequest};

use super::{
    ScoreBy,
    robustness::{backtest, closed_trades, monte_carlo, percentile, pool},
    settings::{OptimizeSettings, Variable},
};
use crate::sweep::label;

/// The most backtests one optimization may run.
pub const MAX_BACKTESTS: usize = 5_000;
/// How many of the best are listed, and checked on the kept-back bars.
pub const TOP: usize = 5;
/// The share of the span's bars values are picked on when some are kept back.
pub const PICKED_ON: f64 = 0.7;

/// One variable's values.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub name: String,
    /// Its place in the strategy's parameters.
    pub index: usize,
    pub values: Vec<f64>,
}

/// Every combination to try: the first variable's values across, the second's down.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub first: Axis,
    pub second: Option<Axis>,
}

/// `variable`'s values, each `step` from `min` up to `max`; rounded for whole-number parameters,
/// without repeats.
///
/// # Errors
///
/// Returns a message saying what's wrong with the range.
pub fn values(param: &Param, variable: &Variable) -> Result<Vec<f64>, String> {
    let name = &variable.name;
    let Variable { min, max, step, .. } = *variable;
    if step <= 0.0 {
        return Err(format!("{name}: Step must be above zero."));
    }
    if min >= max {
        return Err(format!("{name}: Max must be above Min."));
    }
    let round = |v: f64| {
        if param.integer {
            v.round()
        } else {
            (v * 10_000.0).round() / 10_000.0
        }
    };
    param
        .check(round(min))
        .and_then(|()| param.check(round(max)))
        .map_err(|error| format!("{name}: {error}."))?;
    let count = ((max - min) / step + 1e-9).floor() as usize + 1;
    if count > MAX_BACKTESTS {
        return Err(format!(
            "{name}: that Step gives {count} values, more than the {MAX_BACKTESTS} allowed. Use a \
             larger Step."
        ));
    }
    let mut out: Vec<f64> = Vec::with_capacity(count);
    for k in 0..count {
        let value = round(min + step * k as f64);
        if out.last() != Some(&value) {
            out.push(value);
        }
    }
    if out.len() < 2 {
        return Err(format!(
            "{name}: that range gives only one value. Use a smaller Step."
        ));
    }
    Ok(out)
}

impl Grid {
    /// The grid `settings` ask for, from a strategy's `params`.
    ///
    /// # Errors
    ///
    /// Returns a message saying what's wrong with the variables.
    pub fn build(settings: &OptimizeSettings, params: &[Param]) -> Result<Self, String> {
        let axis = |variable: &Variable| {
            let index = params
                .iter()
                .position(|p| p.name == variable.name.as_str())
                .ok_or_else(|| format!("This strategy has no variable {}.", variable.name))?;
            Ok::<_, String>(Axis {
                name: variable.name.clone(),
                index,
                values: values(&params[index], variable)?,
            })
        };
        let [first, second] = &settings.variables;
        let Some(first) = first else {
            return Err("Pick a variable to optimize.".into());
        };
        if second.as_ref().is_some_and(|s| s.name == first.name) {
            return Err("Pick two different variables.".into());
        }
        Ok(Self {
            first: axis(first)?,
            second: second.as_ref().map(axis).transpose()?,
        })
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.first.values.len()
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.second.as_ref().map_or(1, |axis| axis.values.len())
    }

    #[must_use]
    pub fn cells(&self) -> usize {
        self.width() * self.height()
    }

    /// The (parameter's place, value) pairs of a combination.
    #[must_use]
    pub fn values_of(&self, cell: usize) -> Vec<(usize, f64)> {
        let mut out = vec![(self.first.index, self.first.values[cell % self.width()])];
        if let Some(second) = &self.second {
            out.push((second.index, second.values[cell / self.width()]));
        }
        out
    }

    /// A combination's values for reading: `12` or `12 / 30`.
    #[must_use]
    pub fn label(&self, cell: usize) -> String {
        self.values_of(cell)
            .iter()
            .map(|(_, value)| label(*value))
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// Which combination these parameters are, if they're in the grid.
    #[must_use]
    pub fn cell_of(&self, params: &[f64]) -> Option<usize> {
        let at = |axis: &Axis| {
            axis.values
                .iter()
                .position(|v| params.get(axis.index) == Some(v))
        };
        let across = at(&self.first)?;
        match &self.second {
            Some(second) => Some(at(second)? * self.width() + across),
            None => Some(across),
        }
    }

    /// How many backtests it takes, not counting the check on kept-back bars.
    #[must_use]
    pub fn backtests(&self, score: ScoreBy, blocks: u32) -> usize {
        match score {
            ScoreBy::RandomBlocks => self.cells() * blocks as usize,
            _ => self.cells(),
        }
    }

    /// What a run comes to: `20 values × 50 blocks = 1,000 backtests`.
    #[must_use]
    pub fn describe(&self, score: ScoreBy, blocks: u32) -> String {
        let total = thousands(self.backtests(score, blocks));
        let cells = thousands(self.cells());
        match score {
            ScoreBy::RandomBlocks => {
                format!("{cells} values × {blocks} blocks = {total} backtests")
            }
            _ => format!("{total} backtests"),
        }
    }
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Where the bars values are picked on end, when the rest is kept back.
#[must_use]
pub fn split_at(from: usize, bars: usize) -> usize {
    from + (bars.saturating_sub(from) as f64 * PICKED_ON).round() as usize
}

/// How a combination did.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// `what` says what's behind the score.
    Scored {
        score: f64,
        what: String,
    },
    /// It made no trades, or too few to draw from, so its score would mean nothing.
    TooFewTrades,
    Failed(String),
}

impl Outcome {
    #[must_use]
    pub const fn score(&self) -> Option<f64> {
        match self {
            Self::Scored { score, .. } => Some(*score),
            _ => None,
        }
    }
}

/// One of the best combinations.
#[derive(Clone, Debug, PartialEq)]
pub struct Ranked {
    pub cell: usize,
    pub score: f64,
    /// Its score averaged with its neighbours', which it's ranked by.
    pub plateau: f64,
    /// Its return on the kept-back bars.
    pub unseen: Option<f64>,
}

impl Ranked {
    /// Whether it made money where it was picked but lost it on bars it hadn't seen.
    #[must_use]
    pub fn fell_apart(&self) -> bool {
        self.score > 0.0 && self.unseen.is_some_and(|unseen| unseen < 0.0)
    }
}

/// What an optimization found.
#[derive(Clone, Debug)]
pub struct OptimizeResult {
    pub grid: Grid,
    pub score: ScoreBy,
    /// One per combination; the first variable's values across, the second's down.
    pub outcomes: Vec<Outcome>,
    pub top: Vec<Ranked>,
    /// The best is on the edge of the range, so better ones may lie beyond it.
    pub on_edge: bool,
    /// Whether some bars were kept back.
    pub held_out: bool,
}

/// Each scored combination with its score averaged with its neighbours' (the 8 around it, or the
/// 2 beside it in a single row), best first. A neighbour without a score doesn't count.
#[must_use]
pub fn rank(scores: &[Option<f64>], width: usize) -> Vec<(usize, f64)> {
    let height = scores.len() / width.max(1);
    let mut ranked: Vec<(usize, f64)> = (0..scores.len())
        .filter(|&cell| scores[cell].is_some())
        .map(|cell| {
            let (x, y) = (cell % width, cell / width);
            let mut around = Vec::with_capacity(9);
            for row in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                for column in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    around.extend(scores[row * width + column]);
                }
            }
            (cell, around.iter().sum::<f64>() / around.len() as f64)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
}

/// Whether a combination is on the edge of the grid.
#[must_use]
pub const fn on_edge(cell: usize, width: usize, height: usize) -> bool {
    let (x, y) = (cell % width, cell / width);
    x == 0 || x == width - 1 || (height > 1 && (y == 0 || y == height - 1))
}

/// An optimization, ready to go to worker threads.
pub struct OptimizeJob {
    /// The chart's backtest, without its bars.
    pub request: BacktestRequest,
    pub candles: Vec<Candle>,
    pub grid: Grid,
    pub score: ScoreBy,
    /// Where the kept-back bars start, if some are.
    pub split: Option<usize>,
    /// What Random blocks scores on; every combination gets the same ones to be comparable.
    pub blocks: Vec<Range<usize>>,
    pub warm_up: usize,
    /// Monte Carlo's draws, the fall that counts as ruin, and the random numbers.
    pub runs: u32,
    pub ruin_at: f64,
    pub seed: u64,
    pub workers: usize,
    pub stop: Arc<AtomicBool>,
}

/// What one backtest of a combination came to.
struct Measured {
    return_: f64,
    drawdown: f64,
    fills: usize,
    /// Monte Carlo's low end and chance of ruin; `None` if the trades were too few.
    draws: Option<(f64, f64)>,
}

fn return_of(report: &BacktestReport) -> f64 {
    let capital = report.starting_balance;
    report.equity.last().copied().unwrap_or(capital) / capital - 1.0
}

impl OptimizeJob {
    /// How many backtests it comes to, the check on kept-back bars included.
    #[must_use]
    pub fn total(&self) -> usize {
        let held = if self.split.is_some() { TOP } else { 0 };
        self.grid.backtests(self.score, self.blocks.len() as u32) + held.min(self.grid.cells())
    }

    /// Runs it all, telling `progress` how many backtests are done. Blocks until they're finished.
    ///
    /// # Errors
    ///
    /// Returns a message if it was stopped. A combination that fails doesn't end the rest.
    pub fn run(self, progress: &(impl Fn(usize) + Sync)) -> Result<OptimizeResult, String> {
        let done = AtomicUsize::new(0);
        let tick = || progress(done.fetch_add(1, Ordering::Relaxed) + 1);
        let cells = 0..self.grid.cells();
        let tasks: Vec<(usize, Option<Range<usize>>)> = if self.score == ScoreBy::RandomBlocks {
            cells
                .flat_map(|cell| self.blocks.iter().map(move |b| (cell, Some(b.clone()))))
                .collect()
        } else {
            cells.map(|cell| (cell, None)).collect()
        };
        let measured = pool(&tasks, self.workers, &self.stop, &tick, &|(cell, block)| {
            self.measure(*cell, block.as_ref())
        });
        if self.stop.load(Ordering::Relaxed) {
            return Err("Stopped.".into());
        }

        let mut by_cell: Vec<Vec<Result<Measured, String>>> =
            (0..self.grid.cells()).map(|_| Vec::new()).collect();
        for ((cell, _), result) in tasks.iter().zip(measured) {
            by_cell[*cell].extend(result);
        }
        let outcomes: Vec<Outcome> = by_cell
            .into_iter()
            .map(|results| self.outcome(results))
            .collect();

        let scores: Vec<Option<f64>> = outcomes.iter().map(Outcome::score).collect();
        let width = self.grid.width();
        let best: Vec<(usize, f64)> = rank(&scores, width).into_iter().take(TOP).collect();
        let unseen = pool(&best, self.workers, &self.stop, &tick, &|(cell, _)| {
            self.split.and_then(|split| self.unseen(*cell, split))
        });
        if self.stop.load(Ordering::Relaxed) {
            return Err("Stopped.".into());
        }
        let top: Vec<Ranked> = best
            .iter()
            .zip(unseen)
            .map(|(&(cell, plateau), unseen)| Ranked {
                cell,
                score: scores[cell].unwrap_or(0.0),
                plateau,
                unseen: unseen.flatten(),
            })
            .collect();
        Ok(OptimizeResult {
            on_edge: top
                .first()
                .is_some_and(|best| on_edge(best.cell, width, self.grid.height())),
            held_out: self.split.is_some(),
            grid: self.grid,
            score: self.score,
            outcomes,
            top,
        })
    }

    fn request_for(&self, cell: usize) -> Result<BacktestRequest, String> {
        let mut request = self.request.clone();
        for (index, value) in self.grid.values_of(cell) {
            request.strategy.set_param(index, value)?;
        }
        Ok(request)
    }

    fn measure(&self, cell: usize, block: Option<&Range<usize>>) -> Result<Measured, String> {
        let mut request = self.request_for(cell)?;
        let end = self.split.unwrap_or(self.candles.len());
        match block {
            Some(block) => {
                let warm_from = block.start.saturating_sub(self.warm_up);
                request.candles = self.candles[warm_from..block.end].to_vec();
                request.trade_from = self.candles[block.start].ts;
            }
            None => request.candles = self.candles[..end].to_vec(),
        }
        let report = backtest(request)?;
        let draws = (self.score == ScoreBy::MonteCarlo)
            .then(|| {
                monte_carlo(
                    &closed_trades(&report),
                    report.starting_balance,
                    self.runs,
                    self.ruin_at,
                    self.seed,
                )
                .ok()
                .map(|drawn| (drawn.low, drawn.ruin))
            })
            .flatten();
        Ok(Measured {
            return_: return_of(&report),
            drawdown: report.max_drawdown,
            fills: report.fills.len(),
            draws,
        })
    }

    fn outcome(&self, results: Vec<Result<Measured, String>>) -> Outcome {
        let mut measured = Vec::with_capacity(results.len());
        for result in results {
            match result {
                Ok(one) => measured.push(one),
                Err(error) => return Outcome::Failed(error),
            }
        }
        // Not trading scores no better or worse than anything: it says nothing at all.
        if measured.iter().all(|m| m.fills == 0) {
            return Outcome::TooFewTrades;
        }
        match self.score {
            ScoreBy::Backtest => {
                let one = &measured[0];
                Outcome::Scored {
                    score: one.return_,
                    what: format!(
                        "Returned {:+.2}%, with a deepest fall of {:.2}% and {} fills.",
                        one.return_ * 100.0,
                        one.drawdown.abs() * 100.0,
                        one.fills,
                    ),
                }
            }
            ScoreBy::MonteCarlo => match measured[0].draws {
                Some((low, ruin)) => Outcome::Scored {
                    score: low,
                    what: format!(
                        "19 runs in 20 returned more than {:+.2}%. {:.1}% of runs were ruined.",
                        low * 100.0,
                        ruin * 100.0,
                    ),
                },
                None => Outcome::TooFewTrades,
            },
            ScoreBy::RandomBlocks => {
                let mut returns: Vec<f64> = measured.iter().map(|m| m.return_).collect();
                returns.sort_by(f64::total_cmp);
                let median = percentile(&returns, 0.5);
                let share =
                    returns.iter().filter(|r| **r > 0.0).count() as f64 / returns.len() as f64;
                Outcome::Scored {
                    score: median,
                    what: format!(
                        "The median block returned {:+.2}%, and {:.0}% of blocks made money.",
                        median * 100.0,
                        share * 100.0,
                    ),
                }
            }
        }
    }

    /// A combination's backtest of the kept-back bars: warmed up on the bars before them without
    /// trading, and starting from the same capital as every other run.
    #[must_use]
    pub fn unseen_request(&self, cell: usize, split: usize) -> Option<BacktestRequest> {
        let mut request = self.request_for(cell).ok()?;
        request.candles = self.candles[split.saturating_sub(self.warm_up)..].to_vec();
        request.trade_from = self.candles.get(split)?.ts;
        Some(request)
    }

    /// A combination's return on the kept-back bars.
    fn unseen(&self, cell: usize, split: usize) -> Option<f64> {
        backtest(self.unseen_request(cell, split)?)
            .ok()
            .as_ref()
            .map(return_of)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    fn param(name: &'static str, integer: bool) -> Param {
        Param {
            name: Cow::Borrowed(name),
            default: 10.0,
            min: 1.0,
            max: 100.0,
            integer,
        }
    }

    fn variable(name: &str, min: f64, max: f64, step: f64) -> Variable {
        Variable {
            name: name.into(),
            min,
            max,
            step,
        }
    }

    #[test]
    fn values_run_from_min_to_max_by_step() {
        let period = param("Fast", true);
        assert_eq!(
            values(&period, &variable("Fast", 10.0, 14.0, 2.0)),
            Ok(vec![10.0, 12.0, 14.0])
        );
        // Max is only reached if a step lands on it.
        assert_eq!(
            values(&period, &variable("Fast", 10.0, 15.0, 2.0)),
            Ok(vec![10.0, 12.0, 14.0])
        );
        // Whole-number parameters round, without repeats.
        assert_eq!(
            values(&period, &variable("Fast", 10.0, 12.0, 0.5)),
            Ok(vec![10.0, 11.0, 12.0]),
        );
        // Fractions survive a step that doesn't fit binary exactly.
        let fraction = param("Size %", false);
        assert_eq!(
            values(&fraction, &variable("Size %", 1.0, 1.3, 0.1)),
            Ok(vec![1.0, 1.1, 1.2, 1.3])
        );
    }

    #[test]
    fn bad_ranges_say_whats_wrong() {
        let period = param("Fast", true);
        let error =
            |min, max, step| values(&period, &variable("Fast", min, max, step)).unwrap_err();
        assert!(error(10.0, 20.0, 0.0).contains("Step must be above zero"));
        assert!(error(20.0, 10.0, 1.0).contains("Max must be above Min"));
        assert!(error(10.0, 20.0, 50.0).contains("only one value"));
        assert!(
            error(0.0, 20.0, 1.0).starts_with("Fast:"),
            "out of its limits"
        );
        assert!(error(10.0, 90.0, 0.001).contains("larger Step"));
    }

    fn settings(first: Option<Variable>, second: Option<Variable>) -> OptimizeSettings {
        OptimizeSettings {
            variables: [first, second],
            ..OptimizeSettings::default()
        }
    }

    #[test]
    fn a_grid_lays_out_its_combinations() {
        let params = [param("Fast", true), param("Slow", true)];
        let grid = Grid::build(
            &settings(
                Some(variable("Slow", 10.0, 12.0, 1.0)),
                Some(variable("Fast", 5.0, 6.0, 1.0)),
            ),
            &params,
        )
        .unwrap();
        assert_eq!((grid.width(), grid.height(), grid.cells()), (3, 2, 6));
        assert_eq!(grid.values_of(4), vec![(1, 11.0), (0, 6.0)]);
        assert_eq!(grid.label(4), "11 / 6");
        assert_eq!(grid.cell_of(&[6.0, 11.0]), Some(4));
        assert_eq!(grid.cell_of(&[6.0, 99.0]), None);
        assert_eq!(grid.backtests(ScoreBy::Backtest, 50), 6);
        assert_eq!(grid.backtests(ScoreBy::RandomBlocks, 50), 300);
        assert_eq!(
            grid.describe(ScoreBy::RandomBlocks, 250),
            "6 values × 250 blocks = 1,500 backtests"
        );
    }

    #[test]
    fn a_grid_refuses_what_makes_no_sense() {
        let params = [param("Fast", true)];
        let error = |first, second| Grid::build(&settings(first, second), &params).unwrap_err();
        assert!(error(None, None).contains("Pick a variable"));
        let same = variable("Fast", 1.0, 5.0, 1.0);
        assert!(error(Some(same.clone()), Some(same)).contains("two different"));
        let gone = variable("Gone", 1.0, 5.0, 1.0);
        assert!(error(Some(gone), None).contains("no variable Gone"));
    }

    #[test]
    fn a_lone_spike_loses_to_a_plateau() {
        let scores: Vec<Option<f64>> = [0.0, 0.0, 10.0, 0.0, 0.0, 5.0, 6.0, 5.0, 0.0]
            .map(Some)
            .to_vec();
        let ranked = rank(&scores, 9);
        assert_eq!(ranked[0].0, 6, "{ranked:?}");
        assert!((ranked[0].1 - 16.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_heatmap_ranks_with_all_eight_neighbours() {
        // A spike in one corner, and a block of good values in the other.
        #[rustfmt::skip]
        let scores = [
            9.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 4.0, 4.0,
            0.0, 0.0, 4.0, 4.0,
        ]
        .map(Some)
        .to_vec();
        let ranked = rank(&scores, 4);
        assert!(ranked[0].0 == 7 || ranked[0].0 == 11, "{ranked:?}");
    }

    #[test]
    fn edges_average_what_they_have_and_scoreless_neighbours_dont_count() {
        let ranked = rank(&[Some(2.0), Some(4.0)], 2);
        assert!(
            ranked
                .iter()
                .all(|(_, plateau)| (*plateau - 3.0).abs() < 1e-9)
        );
        let ranked = rank(&[None, Some(4.0), None], 3);
        assert_eq!(ranked, vec![(1, 4.0)], "only what has a score is ranked");
    }

    #[test]
    fn the_edge_of_the_grid_is_found() {
        assert!(on_edge(0, 5, 1) && on_edge(4, 5, 1));
        assert!(!on_edge(2, 5, 1));
        assert!(on_edge(1, 3, 3), "the top row");
        assert!(!on_edge(4, 3, 3), "the middle");
    }

    #[test]
    fn the_split_keeps_the_last_part_of_the_span_back() {
        assert_eq!(split_at(0, 100), 70);
        assert_eq!(split_at(500, 1_500), 1_200, "warm-up bars aren't counted");
    }

    #[test]
    fn falling_apart_is_making_money_then_losing_it() {
        let ranked = |score, unseen| Ranked {
            cell: 0,
            score,
            plateau: score,
            unseen,
        };
        assert!(ranked(0.1, Some(-0.01)).fell_apart());
        assert!(!ranked(0.1, Some(0.01)).fell_apart());
        assert!(!ranked(-0.1, Some(-0.01)).fell_apart());
        assert!(!ranked(0.1, None).fell_apart());
    }
}
