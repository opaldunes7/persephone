//! Following any test's trades on a chart. A test's run opens in a drawer beside it, found again
//! where the test kept only its figures, and clicking a trade does what the calendar's replay does:
//! the playhead goes to just before it opened, on a chart of its market, and Play shows it happen.

use persephone_chart::{FillMark, PositionSpan, Trades};
use persephone_core::Side;

use super::*;
use crate::backtest::{
    InspectWhat, Inspected, Inspection, OptimizeRun, TestRun,
    trades::{Ledger, LedgerRow},
};

impl Persephone {
    /// Opens `what`'s trades beside the test, running it again first if the test didn't keep them.
    pub(crate) fn inspect(&mut self, what: InspectWhat) -> Task<Message> {
        let Some(id) = self.panel.as_ref().map(|panel| panel.tile) else {
            return Task::none();
        };
        self.close_inspection();
        let Some(tile) = self.tiles.get(&id) else {
            return Task::none();
        };
        let symbol = tile.market.symbol.clone();
        match what.clone() {
            InspectWhat::Backtest => {
                let Some(Run::Done(report)) = tile.strategy.as_ref().map(|s| &s.run) else {
                    return Task::none();
                };
                let ledger = Ledger::from_report(report, &symbol);
                self.show_ledger(what, "The backtest's trades".into(), ledger, Vec::new());
                Task::none()
            }
            InspectWhat::Listed => {
                if let Some((title, ledger)) = self.listed_trades() {
                    self.show_ledger(what, title, ledger, Vec::new());
                }
                Task::none()
            }
            InspectWhat::Portfolio => {
                let Some(TestRun::Done(found)) = self.panel.as_ref().map(|p| &p.tests.portfolio)
                else {
                    return Task::none();
                };
                let ledger = Ledger::from_portfolio(found);
                let venues = self.leg_venues();
                self.show_ledger(what, "The portfolio's trades".into(), ledger, venues);
                Task::none()
            }
            InspectWhat::Optimized(cell) => self.inspect_optimized(id, cell),
            InspectWhat::WalkWindow(window) => self.inspect_walk_window(id, window),
            InspectWhat::PortfolioSweep(label) => self.inspect_portfolio(Some(label), None),
            InspectWhat::PortfolioWalkWindow(window) => self.inspect_portfolio(None, Some(window)),
            InspectWhat::RandomPlacing(value) => self.inspect_placing(id, false, value),
            InspectWhat::ShiftedPlacing(value) => self.inspect_placing(id, true, value),
            InspectWhat::PortfolioRandomPlacing(value) => self.inspect_portfolio_placing(value),
        }
    }

    /// The portfolio's placing at random whose return was nearest `value`, run again alone.
    fn inspect_portfolio_placing(&mut self, value: f64) -> Task<Message> {
        use backtest::{portfolio, portfolio_tests};
        let Some(panel) = self.panel.as_ref() else {
            return Task::none();
        };
        let (TestRun::Done(found), TestRun::Done(ran)) =
            (&panel.tests.portfolio_random, &panel.tests.portfolio)
        else {
            return Task::none();
        };
        let Some(draw) = found.draw_near(value) else {
            return Task::none();
        };
        let returned = found
            .by_draw
            .iter()
            .find(|(known, _)| *known == draw)
            .map_or(value, |(_, returned)| *returned);
        let (seed, ran) = (panel.tests.seed, Arc::clone(ran));
        let what = InspectWhat::PortfolioRandomPlacing(value);
        let title = format!(
            "Placing {}, which returned {:+.2}%",
            draw + 1,
            returned * 100.0
        );
        let inputs = match self.portfolio_inputs() {
            Ok(inputs) => inputs,
            Err(error) => {
                if let Some(panel) = self.panel.as_mut() {
                    panel.inspect = Some(Inspection::new(what, title, Inspected::Failed(error)));
                }
                return Task::none();
            }
        };
        let venues = self.leg_venues();
        self.find_trades(what, title, venues, move || {
            let placed = portfolio_tests::RandomPortfolioJob {
                report: Arc::new(ran.report.clone()),
                request: inputs.request()?,
                from_index: ran.from_index,
                runs: 1,
                seed,
                workers: 1,
                stop: Arc::default(),
            }
            .placing(draw)?;
            let run = portfolio::run(placed, false)?;
            Ok(Ledger::from_portfolio(&run))
        })
    }

    /// Trades already in hand, ready to follow.
    fn show_ledger(
        &mut self,
        what: InspectWhat,
        title: String,
        ledger: Ledger,
        venues: Vec<(String, Option<String>)>,
    ) {
        if let Some(panel) = self.panel.as_mut() {
            let mut inspection = Inspection::new(what, title, Inspected::Ready(Arc::new(ledger)));
            inspection.venues = venues;
            panel.inspect = Some(inspection);
        }
    }

    /// The trades Monte Carlo or Random blocks shows beside its lines: a draw's, or a block's.
    fn listed_trades(&self) -> Option<(String, Ledger)> {
        let panel = self.panel.as_ref()?;
        let tile = self.tiles.get(&panel.tile)?;
        let symbol = &tile.market.symbol;
        let tests = &panel.tests;
        match tile.backtest.test {
            backtest::TestKind::MonteCarlo => {
                let Some(Run::Done(report)) = tile.strategy.as_ref().map(|s| &s.run) else {
                    return None;
                };
                let result = tests.monte_carlo.as_ref()?.as_ref().ok()?;
                let run = if tests.runs_table {
                    tests.run?
                } else {
                    result.run_of_line(tests.line?)?
                };
                let trades = backtest::robustness::round_trips(report);
                let drawn = result
                    .draws_of_run(run)?
                    .into_iter()
                    .filter_map(|at| trades.get(at).cloned())
                    .collect();
                let title = if run == result.all_stats.len() {
                    "The backtest's trades".to_owned()
                } else {
                    format!("Monte Carlo run {}: its draws", run + 1)
                };
                Some((
                    title,
                    Ledger::from_trips(drawn, symbol, report.starting_balance),
                ))
            }
            backtest::TestKind::RandomBlocks => {
                let backtest::BlockRun::Done(summary) = &tests.blocks else {
                    return None;
                };
                let block = if tests.runs_table {
                    tests.run?
                } else {
                    tests.line?
                };
                let trades = summary.trades.get(block)?.clone();
                let capital = tile.backtest.capital;
                Some((
                    format!("Block {}: its trades", block + 1),
                    Ledger::from_trips(trades, symbol, capital),
                ))
            }
            _ => None,
        }
    }

    /// Each portfolio leg's venue, by its symbol, for opening a chart of it.
    fn leg_venues(&self) -> Vec<(String, Option<String>)> {
        let Some(tile) = self.panel.as_ref().and_then(|p| self.tiles.get(&p.tile)) else {
            return Vec::new();
        };
        tile.backtest
            .portfolio
            .legs
            .iter()
            .map(|leg| {
                (
                    leg.symbol.to_uppercase(),
                    leg.venue.clone().or_else(|| tile.market.local.clone()),
                )
            })
            .collect()
    }

    /// An optimization's value, run again over the bars it was scored on.
    fn inspect_optimized(&mut self, id: TileId, cell: usize) -> Task<Message> {
        let Some(OptimizeRun::Done(found)) = self.panel.as_ref().map(|p| &p.tests.optimize) else {
            return Task::none();
        };
        let found = Arc::clone(found);
        let Some((_, mut request)) = self.backtest_request(id) else {
            return Task::none();
        };
        let Some(tile) = self.tiles.get(&id) else {
            return Task::none();
        };
        let symbol = tile.market.symbol.clone();
        let hold_out = tile.backtest.hold_out_percent / 100.0;
        let from = span_start(&request.candles, tile.market.span);
        let title = format!("{}: {}", names_of(&found.grid), found.grid.label(cell));
        let what = InspectWhat::Optimized(cell);
        self.find_trades(what, title, Vec::new(), move || {
            for (index, value) in found.grid.values_of(cell) {
                request.strategy.set_param(index, value)?;
            }
            // Where it was scored, as its score says: before the kept-back bars, if any were.
            if found.held_out {
                let split = backtest::optimize::split_at(from, request.candles.len(), hold_out);
                request.candles.truncate(split);
            }
            request.ticks = None;
            request.fill_bars = None;
            let report = persephone_engine::run_backtest(&request).map_err(|e| e.to_string())?;
            Ok(Ledger::from_report(&report, &symbol))
        })
    }

    /// A walk-forward window's pick, run again on its unseen stretch alone.
    fn inspect_walk_window(&mut self, id: TileId, at: usize) -> Task<Message> {
        let Some(TestRun::Done(found)) = self.panel.as_ref().map(|p| &p.tests.walk_forward) else {
            return Task::none();
        };
        let Some(window) = found.windows.get(at).cloned() else {
            return Task::none();
        };
        let Some(picked) = window.picked.clone() else {
            return Task::none();
        };
        let Some((_, mut request)) = self.backtest_request(id) else {
            return Task::none();
        };
        let Some(tile) = self.tiles.get(&id) else {
            return Task::none();
        };
        let symbol = tile.market.symbol.clone();
        let warm_up = tile.backtest.warm_up_bars as usize;
        let title = format!("Window {}: {} on its unseen stretch", at + 1, picked.label);
        self.find_trades(InspectWhat::WalkWindow(at), title, Vec::new(), move || {
            for (name, value) in &picked.values {
                let index = request
                    .strategy
                    .info
                    .params
                    .iter()
                    .position(|param| param.name == name.as_str())
                    .ok_or_else(|| format!("The strategy has no {name} now."))?;
                request.strategy.set_param(index, *value)?;
            }
            let candles = std::mem::take(&mut request.candles);
            let from = candles.partition_point(|c| c.ts < window.test_from);
            let to = candles.partition_point(|c| c.ts <= window.test_to);
            if from >= to {
                return Err("Those bars aren't on the chart any more.".into());
            }
            request.candles = candles[from.saturating_sub(warm_up)..to].to_vec();
            request.trade_from = window.test_from;
            request.ticks = None;
            request.fill_bars = None;
            let report = persephone_engine::run_backtest(&request).map_err(|e| e.to_string())?;
            Ok(Ledger::from_report(&report, &symbol))
        })
    }

    /// A portfolio sweep's setup, or a portfolio walk-forward window's pick on its unseen stretch,
    /// run again.
    fn inspect_portfolio(&mut self, label: Option<String>, window: Option<usize>) -> Task<Message> {
        use backtest::{portfolio, portfolio_tests};
        let walked =
            window.and_then(
                |at| match self.panel.as_ref().map(|p| &p.tests.portfolio_walk) {
                    Some(TestRun::Done(found)) => {
                        let last = found.windows.len().saturating_sub(1);
                        found
                            .windows
                            .get(at)
                            .cloned()
                            .map(|window| (at, window, at == last))
                    }
                    _ => None,
                },
            );
        let label = match (&label, &walked) {
            (Some(label), _) => label.clone(),
            (None, Some((_, window, _))) => match &window.picked {
                Some(picked) => picked.label.clone(),
                None => return Task::none(),
            },
            (None, None) => return Task::none(),
        };
        let inputs = match self.portfolio_inputs() {
            Ok(inputs) => inputs,
            Err(error) => {
                let title = format!("{label}: its trades");
                let what = match window {
                    Some(at) => InspectWhat::PortfolioWalkWindow(at),
                    None => InspectWhat::PortfolioSweep(label),
                };
                if let Some(panel) = self.panel.as_mut() {
                    panel.inspect = Some(Inspection::new(what, title, Inspected::Failed(error)));
                }
                return Task::none();
            }
        };
        let Some(vary) = self.panel.as_ref().map(|p| p.tests.portfolio_vary) else {
            return Task::none();
        };
        let venues = self.leg_venues();
        let title = match &walked {
            Some((at, _, _)) => format!("Window {}: {label} on its unseen stretch", at + 1),
            None => format!("{}: {label}", vary.name(&inputs.setup)),
        };
        let what = match window {
            Some(at) => InspectWhat::PortfolioWalkWindow(at),
            None => InspectWhat::PortfolioSweep(label.clone()),
        };
        self.find_trades(what, title, venues, move || {
            let values = portfolio_tests::range_of(
                inputs.settings.sweep_min,
                inputs.settings.sweep_max,
                inputs.settings.sweep_step,
            )?;
            let candidate = inputs
                .candidates(vary, &values)?
                .into_iter()
                .find(|candidate| candidate.label == label)
                .ok_or_else(|| format!("{label} isn't one of the values any more."))?;
            let request = match &walked {
                Some((_, window, last)) => {
                    // The last window runs to the last bar, which it names; the others to the
                    // next one's start.
                    let to = if *last {
                        window.test_to + 1
                    } else {
                        window.test_to
                    };
                    let warm_up = inputs.settings.warm_up_bars as usize;
                    portfolio_tests::over(&candidate.request, window.test_from, to, warm_up)
                }
                None => candidate.request,
            };
            let run = portfolio::run(request, false)?;
            Ok(Ledger::from_portfolio(&run))
        })
    }

    /// The random placing, or the shift of the backtest's timing, whose return was nearest
    /// `value`, run again alone.
    fn inspect_placing(&mut self, id: TileId, shifted: bool, value: f64) -> Task<Message> {
        let Some(panel) = self.panel.as_ref() else {
            return Task::none();
        };
        let found = if shifted {
            &panel.tests.shifted
        } else {
            &panel.tests.random_entries
        };
        let TestRun::Done(found) = found else {
            return Task::none();
        };
        let Some(draw) = found.draw_near(value) else {
            return Task::none();
        };
        let returned = found
            .by_draw
            .iter()
            .find(|(known, _)| *known == draw)
            .map_or(value, |(_, returned)| *returned);
        let seed = panel.tests.seed;
        let Some((_, request)) = self.backtest_request(id) else {
            return Task::none();
        };
        let Some(tile) = self.tiles.get(&id) else {
            return Task::none();
        };
        let Some(Run::Done(report)) = tile.strategy.as_ref().map(|s| &s.run) else {
            return Task::none();
        };
        let report = Arc::clone(report);
        let symbol = tile.market.symbol.clone();
        let from_index = span_start(&request.candles, tile.market.span);
        let what = if shifted {
            InspectWhat::ShiftedPlacing(value)
        } else {
            InspectWhat::RandomPlacing(value)
        };
        let title = format!(
            "{} {}, which returned {:+.2}%",
            if shifted { "Shift" } else { "Placing" },
            draw + 1,
            returned * 100.0
        );
        self.find_trades(what, title, Vec::new(), move || {
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let placed = if shifted {
                backtest::portfolio_tests::ShiftedJob {
                    report,
                    request,
                    from_index,
                    runs: 1,
                    seed,
                    workers: 1,
                    stop,
                }
                .placing(draw)?
            } else {
                backtest::positions::RandomEntriesJob {
                    report,
                    request,
                    from_index,
                    runs: 1,
                    seed,
                    workers: 1,
                    stop,
                }
                .placing(draw)?
            };
            let run = persephone_engine::run_backtest(&placed).map_err(|e| e.to_string())?;
            Ok(Ledger::from_report(&run, &symbol))
        })
    }

    /// Starts `job` on a thread of its own, with the drawer saying it's finding the trades.
    fn find_trades(
        &mut self,
        what: InspectWhat,
        title: String,
        venues: Vec<(String, Option<String>)>,
        job: impl FnOnce() -> Result<Ledger, String> + Send + 'static,
    ) -> Task<Message> {
        let run = backtest::next_run();
        if let Some(panel) = self.panel.as_mut() {
            let mut inspection = Inspection::new(what, title, Inspected::Finding(run));
            inspection.venues = venues;
            panel.inspect = Some(inspection);
        }
        Task::perform(on_thread(move || job().map(Arc::new)), move |result| {
            Message::Inspected(run, result)
        })
    }

    /// What a run found again came to, if the drawer's still waiting for it.
    pub(crate) fn inspected(&mut self, run: u64, result: Result<Arc<Ledger>, String>) {
        if let Some(inspection) = self
            .panel
            .as_mut()
            .and_then(|panel| panel.inspect.as_mut())
            .filter(
                |inspection| matches!(inspection.found, Inspected::Finding(asked) if asked == run),
            )
        {
            inspection.found = match result {
                Ok(ledger) => Inspected::Ready(ledger),
                Err(error) => Inspected::Failed(error),
            };
        }
    }

    /// Follows the inspection's trade `at`: on the panel's chart if it's of that market, or else
    /// on a chart of its own, opened beside it the first time.
    pub(crate) fn inspect_trade(&mut self, at: usize) -> Task<Message> {
        let Some(panel) = self.panel.as_ref() else {
            return Task::none();
        };
        let Some(inspection) = panel.inspect.as_ref() else {
            return Task::none();
        };
        let Inspected::Ready(ledger) = &inspection.found else {
            return Task::none();
        };
        let Some(row) = ledger.rows.get(at) else {
            return Task::none();
        };
        let symbol = row.symbol.clone();
        let venue = inspection
            .venues
            .iter()
            .find(|(known, _)| *known == symbol)
            .map(|(_, venue)| venue.clone());
        let (target, task) = self.chart_for(&symbol, venue);
        let Some(panel) = self.panel.as_mut() else {
            return task;
        };
        let previous = panel.replay_tile.unwrap_or(panel.tile);
        panel.replay_tile = (target != panel.tile).then_some(target);
        if let Some(inspection) = panel.inspect.as_mut() {
            inspection.picked = Some(at);
        }
        if previous != target {
            // The chart the playback left goes back to its own trades, whole.
            self.forget_inspected(previous);
        }
        self.show_picked_trade();
        task
    }

    /// The tile showing `symbol` for the panel: its own if that's its market, else one already
    /// opened for it, else a new one beside it.
    fn chart_for(
        &mut self,
        symbol: &str,
        venue: Option<Option<String>>,
    ) -> (TileId, Task<Message>) {
        let Some(own) = self.panel.as_ref().map(|panel| panel.tile) else {
            return (0, Task::none());
        };
        let Some(chart) = self.tiles.get(&own).map(|tile| tile.market.clone()) else {
            return (own, Task::none());
        };
        if chart.symbol == symbol {
            return (own, Task::none());
        }
        let market = Market {
            symbol: symbol.to_owned(),
            local: venue.unwrap_or_else(|| chart.local.clone()),
            ..chart
        };
        if let Some((&id, _)) = self.tiles.iter().find(|(_, tile)| tile.market == market) {
            return (id, Task::none());
        }
        // Split the panel's own chart in half, Hyprland-style, whatever else has focus: the new
        // market takes its right or bottom half.
        self.layout.focus(own);
        let area = self.tiling_area();
        match self.layout.insert(area) {
            Ok(id) => {
                let task = self.add_tile(id, market, Vec::new(), None);
                self.relayout(true);
                if let Some(tile) = self.tiles.get_mut(&id) {
                    tile.motion = Motion::pop_in(tile.motion.target(), self.now);
                }
                self.save();
                (id, task)
            }
            Err(_) => {
                self.notify(
                    format!(
                        "There's no room for a chart of {symbol}: close one to follow its trades."
                    ),
                    true,
                );
                (own, Task::none())
            }
        }
    }

    /// Puts the picked trade on its chart: every trade of that market as markers, the picked one
    /// lit, and the playhead just before it opened. Waits, if the chart's bars aren't in yet; they
    /// call this again when they are.
    pub(crate) fn show_picked_trade(&mut self) {
        let Some(panel) = self.panel.as_ref() else {
            return;
        };
        let target = panel.replay_tile.unwrap_or(panel.tile);
        let Some(inspection) = panel.inspect.as_ref() else {
            return;
        };
        let (Inspected::Ready(ledger), Some(at)) = (&inspection.found, inspection.picked) else {
            return;
        };
        let ledger = Arc::clone(ledger);
        let Some(tile) = self.tiles.get_mut(&target) else {
            return;
        };
        let Some(chart) = tile.chart.as_mut() else {
            return;
        };
        let Some(picked) = ledger.rows.get(at) else {
            return;
        };
        let candles = &chart.data().candles;
        let marks = marks_of(&ledger.rows, &tile.market.symbol, candles);
        let mut trades = Trades::from_fills(&marks, candles.len());
        let entry = bar_of(candles, picked.trip.entry_ts);
        trades.highlight = Some(PositionSpan {
            from: entry,
            to: if picked.trip.open {
                candles.len().saturating_sub(1)
            } else {
                bar_of(candles, picked.trip.exit_ts)
            }
            .max(entry),
            long: picked.trip.long,
        });
        tile.inspected = Some(trades.clone());
        chart.set_trades(Some(trades));
        // As the calendar's replay does: paused, a few bars before it opened.
        let Some((start, end)) = self.playback_span() else {
            return;
        };
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let lead_in = entry.saturating_sub(backtest::REPLAY_LEAD_BARS);
        let Some(timeline) = panel.timeline.as_ref() else {
            return;
        };
        let at = timeline.closes.get(lead_in).copied().unwrap_or(start);
        panel.playback.pause();
        panel.playback.at = Some(at.clamp(start, end));
        self.sync_playhead();
        if let Some(chart) = self.tiles.get_mut(&target).and_then(|t| t.chart.as_mut()) {
            chart.show_playhead();
        }
    }

    /// Closes the drawer: every chart it was showing on goes back to its own trades, whole.
    pub(crate) fn close_inspection(&mut self) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        if panel.inspect.take().is_none() && panel.replay_tile.is_none() {
            return;
        }
        let own = panel.tile;
        let replay = panel.replay_tile.take();
        panel.playback.reset();
        self.forget_inspected(own);
        if let Some(other) = replay {
            self.forget_inspected(other);
        }
        self.sync_playhead();
    }

    /// Tile `id` shows its own trades again, and all its bars.
    fn forget_inspected(&mut self, id: TileId) {
        let own = self.panel.as_ref().map(|panel| panel.tile);
        if let Some(tile) = self.tiles.get_mut(&id) {
            tile.inspected = None;
            // The panel's own chart keeps its playhead; any other has none of its own.
            if own != Some(id)
                && let Some(chart) = tile.chart.as_mut()
            {
                chart.set_playhead(None);
            }
        }
        self.restudy(id);
    }
}

/// Which bar a fill at `ts` belongs to: the one whose close set it off, or at the next open, the
/// one opening, as backtests count them.
fn bar_of(candles: &[Candle], ts: UnixNanos) -> usize {
    candles
        .partition_point(|candle| candle.ts < ts)
        .saturating_sub(1)
        .min(candles.len().saturating_sub(1))
}

/// Markers for `symbol`'s trades: an entry and, once closed, an exit each, once each though a
/// draw may repeat them.
fn marks_of(rows: &[LedgerRow], symbol: &str, candles: &[Candle]) -> Vec<FillMark> {
    let mut trips: Vec<&LedgerRow> = rows.iter().filter(|row| row.symbol == symbol).collect();
    trips.sort_by_key(|row| (row.trip.entry_ts, row.trip.exit_ts));
    trips.dedup_by_key(|row| (row.trip.entry_ts, row.trip.exit_ts, row.trip.long));
    let mut marks = Vec::with_capacity(trips.len() * 2);
    for row in trips {
        let trip = &row.trip;
        let (opening, closing) = if trip.long {
            (Side::Buy, Side::Sell)
        } else {
            (Side::Sell, Side::Buy)
        };
        marks.push(FillMark {
            bar: bar_of(candles, trip.entry_ts),
            side: opening,
            price: trip.entry_price,
            qty: trip.qty,
        });
        if !trip.open {
            marks.push(FillMark {
                bar: bar_of(candles, trip.exit_ts),
                side: closing,
                price: trip.exit_price,
                qty: trip.qty,
            });
        }
    }
    // In time; a reversal's close comes before its next open on the same bar.
    marks.sort_by_key(|mark| mark.bar);
    marks
}

/// An optimization's variables' names: `Fast` or `Fast / Slow`.
fn names_of(grid: &backtest::optimize::Grid) -> String {
    match &grid.second {
        Some(second) => format!("{} / {}", grid.first.name, second.name),
        None => grid.first.name.clone(),
    }
}
