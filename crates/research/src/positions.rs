//! A strategy given as a list: which position to hold from which bar. It's the simple way in for
//! predictions made elsewhere, such as by a model trained in Python.
//!
//! ```json
//! {"positions": [[1704067200000000000, 1], [1704153600000000000, -1], [1704240000000000000, 0]]}
//! ```
//!
//! A time is a bar's `ts`, as `get_bars` and exports give it: when the bar opens. Its order fills at
//! that bar's close, as any strategy's does, and holds until the next entry. Nothing stops a list looking ahead: that's up to whoever
//! made it.

use std::{
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use persephone_core::{
    Candle, UnixNanos,
    signal::{Signal, SignalFactory},
    strategy::{StrategyInfo, StrategyKind, StrategySpec},
};
use persephone_engine::{BacktestReport, BacktestRequest};
use rand::{RngExt as _, SeedableRng as _, rngs::StdRng, seq::SliceRandom};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    results::with_params,
    robustness::{backtest, percentile, pool, round_trips},
};

/// Wanted positions by time, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Positions {
    list: Arc<Vec<(UnixNanos, i8)>>,
}

impl Positions {
    /// A list already worked out, in order.
    #[must_use]
    pub fn from_list(list: Vec<(UnixNanos, i8)>) -> Self {
        Self {
            list: Arc::new(list),
        }
    }

    /// Reads a list of `[ts, position]`, or the path of a JSON file holding one, as a long list
    /// costs an agent a lot to write out in a call.
    ///
    /// # Errors
    ///
    /// Returns a message saying which entry is wrong and why.
    pub fn parse(source: &Value) -> Result<Self, String> {
        if let Some(path) = source.as_str() {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("positions: couldn't read {path}: {e}"))?;
            let list: Value = serde_json::from_str(&text)
                .map_err(|e| format!("positions: {path} isn't JSON: {e}"))?;
            return Self::parse(&list);
        }
        let items = source
            .as_array()
            .filter(|items| !items.is_empty())
            .ok_or("positions: expected a list of [ts, position], with at least one")?;
        let mut list: Vec<(UnixNanos, i8)> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let entry = item
                .as_array()
                .filter(|pair| pair.len() == 2)
                .and_then(|pair| Some((pair[0].as_u64()?, pair[1].as_i64()?)))
                .filter(|(_, position)| (-1..=1).contains(position))
                .ok_or_else(|| {
                    format!(
                        "positions[{i}]: expected [ts, position], with ts in nanoseconds and position 1, -1 or 0, not {item}"
                    )
                })?;
            if list.last().is_some_and(|last| last.0 >= entry.0) {
                return Err(format!(
                    "positions[{i}]: times should only go up, and {} doesn't",
                    entry.0
                ));
            }
            list.push((entry.0, entry.1 as i8));
        }
        Ok(Self {
            list: Arc::new(list),
        })
    }

    /// The same list by when bars close, which is when a strategy hears of them, from the times
    /// bars open and close.
    ///
    /// # Errors
    ///
    /// Returns a message naming a time that isn't a bar's.
    pub fn at_closes(&self, opens: &[UnixNanos], closes: &[UnixNanos]) -> Result<Self, String> {
        let (Some(&first), Some(&last)) = (opens.first(), opens.last()) else {
            return Err("positions: there are no bars to place them on".into());
        };
        // A list can be made for more than this span: what's after it doesn't matter, and what's
        // before it is taken up by its first bar.
        let list = self
            .list
            .iter()
            .filter(|(ts, _)| *ts <= last)
            .map(|&(ts, position)| {
                if ts < first {
                    return Ok((closes[0], position));
                }
                let found = opens.binary_search(&ts).map_err(|_| {
                    format!(
                        "positions: no bar opens at {ts}; use the `ts` of a bar from get_bars, here from {} to {}",
                        opens.first().copied().unwrap_or(0),
                        opens.last().copied().unwrap_or(0),
                    )
                })?;
                Ok((closes[found], position))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            list: Arc::new(list),
        })
    }

    /// The list as [`Self::parse`] reads it, with any file's contents written out.
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Array(
            self.list
                .iter()
                .map(|&(ts, position)| serde_json::json!([ts, position]))
                .collect(),
        )
    }

    /// A short id for this exact list.
    #[must_use]
    pub fn id(&self) -> String {
        let mut digest = Sha256::new();
        for (ts, position) in self.list.iter() {
            digest.update(ts.to_le_bytes());
            digest.update(position.to_le_bytes());
        }
        let mut id = String::from("positions-");
        for byte in &digest.finalize()[..8] {
            let _ = write!(id, "{byte:02x}");
        }
        id
    }

    #[must_use]
    pub fn info(&self) -> StrategyInfo {
        StrategyInfo::new("Positions", "A list of positions by time", Vec::new())
    }

    /// The strategy to put in a backtest request, with this as its plugin.
    #[must_use]
    pub fn spec(&self) -> StrategySpec {
        StrategySpec::from_info(StrategyKind::Plugin(self.id()), Arc::new(self.info()), &[])
    }
}

impl SignalFactory for Positions {
    fn create(&self, _params: &[f64]) -> Result<Box<dyn Signal>, String> {
        Ok(Box::new(Running {
            list: Arc::clone(&self.list),
            next: 0,
            wanted: None,
        }))
    }
}

/// Random entries with the same holds as `holds` (each a direction and how many bars), in a random
/// order and with random gaps, over the bars `first..end`. Gives each change of position's bar.
/// Nothing overlaps. `None` if they don't fit.
pub fn random_entries(
    holds: &[(i8, usize)],
    first: usize,
    end: usize,
    seed: u64,
) -> Option<Vec<(usize, i8)>> {
    let mut rng = StdRng::seed_from_u64(seed);
    let rng = &mut rng;
    let held: usize = holds.iter().map(|hold| hold.1).sum();
    let spare = end.checked_sub(first)?.checked_sub(held)?;
    let mut order: Vec<(i8, usize)> = holds.to_vec();
    order.shuffle(rng);
    // Where the gaps fall: as many cuts of the spare bars as there are holds.
    let mut cuts: Vec<usize> = (0..order.len())
        .map(|_| rng.random_range(0..=spare))
        .collect();
    cuts.sort_unstable();
    let mut changes = Vec::with_capacity(order.len() * 2);
    let mut used = 0;
    for ((direction, bars), cut) in order.into_iter().zip(cuts) {
        let entry = first + cut + used;
        // Right after the last, the entry itself ends what came before.
        if changes
            .last()
            .is_some_and(|last: &(usize, i8)| last.0 == entry)
        {
            changes.pop();
        }
        changes.push((entry, direction));
        used += bars;
        changes.push((entry + bars, 0));
    }
    // The last can run to the end, with no bar to close on.
    if changes.last().is_some_and(|last| last.0 >= end) {
        changes.pop();
    }
    Some(changes)
}

struct Running {
    list: Arc<Vec<(UnixNanos, i8)>>,
    next: usize,
    wanted: Option<i8>,
}

impl Signal for Running {
    fn on_bar(&mut self, bar: &Candle) -> Result<Option<i8>, String> {
        while let Some(&(ts, position)) = self.list.get(self.next)
            && ts <= bar.ts
        {
            self.wanted = Some(position);
            self.next += 1;
        }
        // Asked for on every bar, as a list says what to hold from then on: a span or random block
        // starting partway through takes up what's wanted by its first bar that can trade.
        Ok(self.wanted)
    }
}

/// A backtest's holds at random times, many times over, ready to go to worker threads.
pub struct RandomEntriesJob {
    pub report: Arc<BacktestReport>,
    /// The backtest, on its bars.
    pub request: BacktestRequest,
    /// Where the span starts in the bars.
    pub from_index: usize,
    pub runs: usize,
    pub seed: u64,
    pub workers: usize,
    pub stop: Arc<AtomicBool>,
}

/// What the same trading comes to with entries owing nothing to the strategy.
#[derive(Clone, Debug, PartialEq)]
pub struct RandomEntries {
    pub runs: usize,
    pub trades: usize,
    pub actual_return: f64,
    pub actual_fell: f64,
    /// The share of random placings that did at least as well as the backtest.
    pub share_matching_return: f64,
    pub share_matching_fall: f64,
    pub median: f64,
    pub low: f64,
    pub high: f64,
    /// What every placing returned and how far it fell, each sorted, to draw.
    pub returns: Vec<f64>,
    pub falls: Vec<f64>,
    /// Each placing's draw and what it returned, to find one again. Empty where they can't be.
    pub by_draw: Vec<(u64, f64)>,
}

impl RandomEntries {
    /// The draw whose return was nearest `value`.
    #[must_use]
    pub fn draw_near(&self, value: f64) -> Option<u64> {
        self.by_draw
            .iter()
            .min_by(|a, b| (a.1 - value).abs().total_cmp(&(b.1 - value).abs()))
            .map(|(draw, _)| *draw)
    }
}

/// What's said when there's too little for the test to mean anything.
pub const ONE_TRADE: &str =
    "With one trade there's little to place at random, so this says almost nothing.";

impl RandomEntriesJob {
    /// Each trade's direction and how many bars it held.
    fn holds(&self) -> Vec<(i8, usize)> {
        let report = &self.report;
        round_trips(report)
            .iter()
            .map(|trade| {
                let bars = if trade.open {
                    report.equity.len().saturating_sub(trade.entry_bar)
                } else {
                    trade.exit_bar.saturating_sub(trade.entry_bar)
                };
                (if trade.long { 1 } else { -1 }, bars.max(1))
            })
            .collect()
    }

    /// The backtest sized as the run was, with positions to come in place of its strategy.
    fn shell(&self) -> Result<BacktestRequest, String> {
        let mut request = self.request.clone();
        request.ticks = None;
        request.fill_bars = None;
        let shell = Positions::from_list(Vec::new());
        request.strategy = with_params(
            shell.spec().kind,
            Arc::new(shell.info()),
            &serde_json::json!({"params": {"Size %": request.strategy.size_fraction() * 100.0}}),
        )?;
        Ok(request)
    }

    fn placed(
        &self,
        shell: &BacktestRequest,
        holds: &[(i8, usize)],
        draw: u64,
    ) -> Result<BacktestRequest, String> {
        let closes = shell.bars.closes(&shell.candles);
        let changes = random_entries(
            holds,
            self.from_index,
            shell.candles.len(),
            self.seed ^ (draw << 16),
        )
        .ok_or("Its trades don't fit the span.")?;
        let list = changes
            .iter()
            .map(|&(bar, position)| (closes[bar], position))
            .collect();
        let mut trying = shell.clone();
        trying.plugin = Some(Arc::new(Positions::from_list(list)));
        Ok(trying)
    }

    /// Placing `draw` alone, as `run` backtests it, to look into its trades.
    ///
    /// # Errors
    ///
    /// Returns a message if the run has no trades or they don't fit.
    pub fn placing(&self, draw: u64) -> Result<BacktestRequest, String> {
        let holds = self.holds();
        if holds.is_empty() {
            return Err("That run made no trades, so there's nothing to place at random.".into());
        }
        self.placed(&self.shell()?, &holds, draw)
    }

    /// # Errors
    ///
    /// Returns a message if the run has no trades or they don't fit, a backtest fails, or it's
    /// stopped before any finish.
    pub fn run(self, progress: &(impl Fn(usize) + Sync)) -> Result<RandomEntries, String> {
        let report = &self.report;
        let holds = self.holds();
        if holds.is_empty() {
            return Err("That run made no trades, so there's nothing to place at random.".into());
        }
        let shell = self.shell()?;
        let seeds: Vec<u64> = (0..self.runs as u64).collect();
        let done = AtomicUsize::new(0);
        let results = pool(
            &seeds,
            self.workers,
            &self.stop,
            &|| progress(done.fetch_add(1, Ordering::Relaxed) + 1),
            &|&draw| {
                backtest(self.placed(&shell, &holds, draw)?).map(|run| {
                    let capital = run.starting_balance;
                    let last = run.equity.last().copied().unwrap_or(capital);
                    (last / capital - 1.0, run.max_drawdown.abs())
                })
            },
        );
        let mut returns = Vec::with_capacity(self.runs);
        let mut falls = Vec::with_capacity(self.runs);
        let mut by_draw = Vec::with_capacity(self.runs);
        for (draw, result) in seeds.iter().zip(results) {
            let Some(result) = result else { continue };
            let (returned, fell) = result?;
            returns.push(returned);
            falls.push(fell);
            by_draw.push((*draw, returned));
        }
        if returns.is_empty() {
            return Err("Stopped.".into());
        }
        returns.sort_by(f64::total_cmp);
        falls.sort_by(f64::total_cmp);
        let capital = report.starting_balance;
        let actual = report.equity.last().copied().unwrap_or(capital) / capital - 1.0;
        let fell = report.max_drawdown.abs();
        let count = returns.len() as f64;
        Ok(RandomEntries {
            runs: returns.len(),
            trades: holds.len(),
            actual_return: actual,
            actual_fell: fell,
            share_matching_return: returns.iter().filter(|r| **r >= actual).count() as f64 / count,
            share_matching_fall: falls.iter().filter(|f| **f <= fell).count() as f64 / count,
            median: percentile(&returns, 0.5),
            low: percentile(&returns, 0.05),
            high: percentile(&returns, 0.95),
            returns,
            falls,
            by_draw,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn bar(ts: UnixNanos) -> Candle {
        Candle {
            ts,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        }
    }

    #[test]
    fn it_asks_for_each_position_on_its_bar_and_keeps_it_between() {
        let list = Positions::parse(&json!([[10, 1], [30, -1], [40, 0]])).unwrap();
        let mut running = list.create(&[]).unwrap();
        let wanted: Vec<Option<i8>> = [5, 10, 20, 30, 40, 50]
            .iter()
            .map(|&ts| running.on_bar(&bar(ts)).unwrap())
            .collect();
        assert_eq!(
            wanted,
            vec![None, Some(1), Some(1), Some(-1), Some(0), Some(0)]
        );
    }

    #[test]
    fn a_run_starting_later_picks_up_what_was_wanted_by_then() {
        let list = Positions::parse(&json!([[10, 1], [20, -1]])).unwrap();
        let mut running = list.create(&[]).unwrap();
        assert_eq!(running.on_bar(&bar(25)).unwrap(), Some(-1));
    }

    #[test]
    fn times_move_from_when_bars_open_to_when_they_close() {
        let list = Positions::parse(&json!([[100, 1], [200, 0]])).unwrap();
        let moved = list.at_closes(&[100, 200], &[199, 299]).unwrap();
        assert_eq!(*moved.list, vec![(199, 1), (299, 0)]);
        let error = list.at_closes(&[100, 300], &[299, 499]).unwrap_err();
        assert!(error.contains("no bar opens at 200"), "{error}");

        // Past either end of the bars, it's kept to what these bars need.
        let long =
            Positions::parse(&json!([[10, 1], [50, -1], [100, 0], [200, 1], [900, -1]])).unwrap();
        let moved = long.at_closes(&[100, 200], &[199, 299]).unwrap();
        assert_eq!(*moved.list, vec![(199, 1), (199, -1), (199, 0), (299, 1)]);
    }

    #[test]
    fn random_entries_keep_the_holds_and_never_overlap() {
        let holds = [(1, 10), (-1, 5), (1, 30), (-1, 1), (1, 20)];
        for seed in 0..200 {
            let changes = random_entries(&holds, 100, 180, seed).unwrap();
            assert!(
                changes.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "{changes:?}"
            );
            assert!(changes.iter().all(|change| (100..180).contains(&change.0)));
            // Bars held in each direction, walking the changes.
            let (mut long, mut short) = (0, 0);
            for (i, &(bar, position)) in changes.iter().enumerate() {
                let until = changes.get(i + 1).map_or(180, |next| next.0);
                match position {
                    1 => long += until - bar,
                    -1 => short += until - bar,
                    _ => {}
                }
            }
            assert_eq!((long, short), (60, 6), "{changes:?}");
        }
        assert!(random_entries(&holds, 0, 10, 0).is_none());
    }

    #[test]
    fn bad_lists_say_what_is_wrong() {
        let error = |list: Value| Positions::parse(&list).unwrap_err();
        assert!(error(json!([])).contains("a list"));
        assert!(error(json!([[1, 2]])).contains("positions[0]"));
        assert!(error(json!([[1, 1], "x"])).contains("positions[1]"));
        assert!(error(json!([[5, 1], [5, 0]])).contains("only go up"));
    }
}
