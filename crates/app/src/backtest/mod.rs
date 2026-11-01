//! The backtest panel across the top of the window, for one chart: its strategy's equity, the
//! span tested (a two-handled slider over the market's whole history), and a playback that
//! reveals the finished backtest bar by bar at any speed from real time up.
//!
//! Playback never reruns anything. The backtest runs once over the span (strategies only see
//! closed bars, so running ahead changes nothing); playing moves a playhead through market time,
//! and the chart, trades, equity and statistics show what had happened by then.
//!
//! Two more tests sit beside it, in [`robustness`]: they say how much of a result was luck.

mod view;
mod widgets;

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use iced::widget::combo_box;
use persephone_chart::ChartData;
use persephone_core::{
    Candle, Timeframe, UnixNanos,
    strategy::StrategyKind,
    tick::{TickFeed, Ticks},
};
use persephone_engine::BacktestReport;
use persephone_tiling::TileId;

pub use persephone_research::{
    Field, RunColumn, WARM_UP_BARS, calendar, figures, logic_draft, optimize, portfolio,
    portfolio_tests, positions, robustness,
    settings::{
        self, BlockLength, Commission, Edit, FillTiming, OneWay, OptimizeSettings, Precision,
        ScoreBy, Section, SessionRule, Settings, TestKind, Variable, parse_time, time_text,
    },
    thousands, trades, walk_forward,
};

pub use self::view::{PLUGIN_SLOTS, PanelInfo, Status, panel, test_tabs};

/// The panel's share of the window when it opens, and the least it's ever given.
pub const DEFAULT_SHARE: f32 = 0.22;
pub const MIN_HEIGHT: f32 = 190.0;
/// The panel's draggable bottom edge.
const EDGE_HEIGHT: f32 = 5.0;
/// What the panel's pane takes around its content: the gap above it and its border.
const PANE_FRAME: f32 = 10.0;
/// What the panel leaves the charts, however much it wants.
const CHARTS_ROOM: f32 = 160.0;
/// At Max speed, a whole span plays in this long.
const MAX_SPEED_SECONDS: f64 = 2.0;
/// Where on the speed slider Max begins.
const MAX_FROM: f32 = 0.95;

/// A chapter of the panel's settings, each opened and closed on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Chapter {
    /// What the test being shown has beyond its main line.
    TestOptions,
    /// A portfolio's markets and how they're held.
    Markets,
    Rebalancing,
    Costs,
    Orders,
    Exits,
    Position,
    Hours,
    Records,
    Plugins,
}

impl Chapter {
    /// Its name, as headed. The test's own options are named after it.
    #[must_use]
    pub fn name(self, test: TestKind) -> &'static str {
        match self {
            Self::TestOptions => match test {
                TestKind::Optimize => "Optimize options",
                TestKind::WalkForward => "Walk-forward options",
                _ => "Options",
            },
            Self::Markets => "Markets",
            Self::Rebalancing => "Rebalancing & margin",
            Self::Costs => "Costs",
            Self::Orders => "Orders & fills",
            Self::Exits => "Exits",
            Self::Position => "Position & account",
            Self::Hours => "Trading hours",
            Self::Records => "Data & records",
            Self::Plugins => "Plugins",
        }
    }

    /// How many of its switches are on, for its heading; `None` for one with none to count.
    #[must_use]
    pub fn on(self, settings: &Settings) -> Option<usize> {
        let count = |list: &[bool]| list.iter().filter(|on| **on).count();
        Some(match self {
            Self::TestOptions | Self::Markets | Self::Rebalancing => return None,
            Self::Costs => count(&[
                settings.no_costs,
                settings.commission.on,
                settings.slippage.on,
                settings.slippage_percent.on,
                settings.spread.on,
                settings.range_slippage_percent.on,
                settings.impact_percent.on,
                settings.borrow_percent_per_year.on,
                settings.cash_interest_percent_per_year.on,
            ]),
            Self::Orders => count(&[
                settings.fill_timing == FillTiming::NextOpen,
                settings.limit.on,
                settings.max_participation_percent.on,
                settings.latency.on,
                settings.latency_jitter.on,
                settings.precision == Precision::High,
            ]),
            Self::Exits => count(&[
                settings.stop_loss_percent.on,
                settings.trailing_stop_percent.on,
                settings.take_profit_percent.on,
            ]),
            Self::Position => count(&[
                settings.compound,
                settings.leverage.on,
                settings.max_gross_percent.on,
                settings.direction.on,
                settings.invert,
            ]),
            Self::Hours => count(&[settings.session.on]),
            Self::Records => count(&[settings.decision_log]),
            Self::Plugins => settings.plugins.len(),
        })
    }
}

/// The panel's state; the chart's settings and span live on its tile.
#[derive(Debug)]
pub struct Panel {
    pub tile: TileId,
    /// How tall it is, in logical pixels.
    pub height: f32,
    pub playback: Playback,
    /// The market's whole history, once known; the error if it couldn't be found.
    pub history: Option<Result<History, String>>,
    /// The market (venue and symbol) whose history was last asked for. An answer for any other
    /// is out of date and dropped.
    pub history_for: Option<(Option<String>, String)>,
    /// The span's handles while they're dragged, as times.
    pub dragging: Option<(UnixNanos, UnixNanos)>,
    /// When the chart's bars close, for the playhead.
    pub timeline: Option<Timeline>,
    /// Ticks read for High precision's forming bars.
    pub ticks: Option<TickCache>,
    /// What's typed in the settings sheet's boxes, until applied.
    pub inputs: Vec<(Field, String)>,
    /// Why the last edit couldn't be applied.
    pub error: Option<String>,
    /// How tall the settings measure, for keeping the panel at least that tall.
    pub settings_height: f32,
    /// Whether exporting everything includes full runs, which can be large.
    pub export_runs: bool,
    /// Showing each limit order in the backtest's pane.
    pub limits_shown: bool,
    pub decisions_shown: bool,
    /// How the last export or import went, and whether it failed.
    pub notice: Option<(String, bool)>,
    /// The search for plugins to add: every kind not already on.
    pub plugin_search: combo_box::State<&'static str>,
    /// The settings' chapters open; all are closed until opened.
    pub open_chapters: std::collections::BTreeSet<Chapter>,
    /// How far the Monte Carlo and Random blocks tests have got.
    pub tests: Tests,
    /// Whether the backtest's calendar is showing, and how far into it.
    pub calendar: calendar::CalendarView,
    /// A test's trades, open beside it to follow on a chart, if any.
    pub inspect: Option<Inspection>,
    /// The chart the playback drives while following a trade on a market the panel's chart isn't
    /// showing. `None` is the panel's own.
    pub replay_tile: Option<TileId>,
    /// The Build tab: what's typed, and how it's shown.
    pub build: BuildState,
}

/// The Build tab's own state. The tree itself is the chart's, saved with it.
#[derive(Debug, Default)]
pub struct BuildState {
    /// The raw JSON rather than the rule rows.
    pub json_mode: bool,
    pub json: iced::widget::text_editor::Content,
    /// What's typed in each number box, by where it is, while it's typed.
    pub typed: Vec<(logic_draft::NodePath, BuildSlot, String)>,
    /// What each variable's name and starting value boxes hold while they're typed.
    pub renaming: Vec<(String, String)>,
    pub values: Vec<(String, String)>,
    /// Why the last change couldn't be made, or why the tree can't run.
    pub error: Option<String>,
    /// The name to save it as.
    pub save_name: String,
    /// What saving last said.
    pub saved: Option<String>,
}

/// Which box at a place in the tree is meant: the number itself, or a look-back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildSlot {
    Number,
    Bars,
}

impl BuildState {
    #[must_use]
    pub fn typed(&self, path: &logic_draft::NodePath, slot: BuildSlot) -> Option<&str> {
        self.typed
            .iter()
            .find(|(at, s, _)| at == path && *s == slot)
            .map(|(_, _, text)| text.as_str())
    }

    pub fn set_typed(&mut self, path: logic_draft::NodePath, slot: BuildSlot, text: String) {
        self.typed.retain(|(at, s, _)| !(*at == path && *s == slot));
        self.typed.push((path, slot, text));
    }
}

/// What the Build tab asks for.
#[derive(Clone, Debug)]
pub enum BuildMessage {
    /// Start again, from nothing or one of the guide's examples (by place in `EXAMPLES`).
    StartFrom(Option<usize>),
    /// Put the tree as it is on the chart, though nothing's changed yet.
    UseOnChart,
    Name(String),
    /// Give a wish its first condition, or take it away.
    Side(logic_draft::Wish, bool),
    /// A condition, or a group of one, into the group at a place.
    Add(logic_draft::NodePath, bool),
    Remove(logic_draft::NodePath),
    /// Make a group need all of its rules, or any one.
    Group(logic_draft::NodePath, bool),
    Not(logic_draft::NodePath),
    Comparison(logic_draft::NodePath, logic_draft::Comparison),
    Kind(logic_draft::NodePath, &'static str),
    Typed(logic_draft::NodePath, BuildSlot, String),
    Timeframe(logic_draft::NodePath, String),
    /// A number into a variable or back (# and $), or a look-back.
    Variable(logic_draft::NodePath, BuildSlot),
    RenameTyped(String, String),
    Rename(String),
    ValueTyped(String, String),
    /// Raw JSON rather than the rules, or back.
    JsonMode(bool),
    JsonEdit(iced::widget::text_editor::Action),
    ApplyJson,
    CopyJson,
    SaveName(String),
    Save,
}

/// One run's trades, picked out of a test to follow on the chart: the backtest's own, a draw or
/// block of them, a value an optimization tried, a walk-forward's pick, or a portfolio's.
#[derive(Debug)]
pub struct Inspection {
    /// What it's of, and that as the drawer heads it.
    pub what: InspectWhat,
    pub title: String,
    pub found: Inspected,
    /// Each market's venue, by its symbol, for a portfolio's legs.
    pub venues: Vec<(String, Option<String>)>,
    /// The row last sent to the chart.
    pub picked: Option<usize>,
    /// Biggest profit first, rather than the order they opened.
    pub by_profit: bool,
}

/// Where an inspection's trades have got to. Some are found by running that one run again.
#[derive(Debug)]
pub enum Inspected {
    Finding(u64),
    Ready(Arc<trades::Ledger>),
    Failed(String),
}

impl Inspection {
    #[must_use]
    pub fn new(what: InspectWhat, title: String, found: Inspected) -> Self {
        Self {
            what,
            title,
            found,
            venues: Vec::new(),
            picked: None,
            by_profit: false,
        }
    }

    /// The order rows show in: each row's place in the ledger.
    #[must_use]
    pub fn order(&self) -> Vec<usize> {
        let Inspected::Ready(ledger) = &self.found else {
            return Vec::new();
        };
        let mut order: Vec<usize> = (0..ledger.rows.len()).collect();
        if self.by_profit {
            order.sort_by(|&a, &b| {
                ledger.rows[b]
                    .trip
                    .profit
                    .total_cmp(&ledger.rows[a].trip.profit)
            });
        }
        order
    }
}

/// What an inspection is of.
#[derive(Clone, Debug, PartialEq)]
pub enum InspectWhat {
    /// The chart's own backtest.
    Backtest,
    /// The trades Monte Carlo or Random blocks lists beside its lines.
    Listed,
    /// A value an optimization tried, by its place in the grid.
    Optimized(usize),
    /// A walk-forward window's pick, on its unseen stretch.
    WalkWindow(usize),
    /// The portfolio run.
    Portfolio,
    /// A portfolio sweep's setup, by its label.
    PortfolioSweep(String),
    /// A portfolio walk-forward window's pick, on its unseen stretch.
    PortfolioWalkWindow(usize),
    /// The random placing, or the shift of the backtest's timing, whose return was nearest this.
    RandomPlacing(f64),
    ShiftedPlacing(f64),
    /// The portfolio's trades placed at random, the placing whose return was nearest this.
    PortfolioRandomPlacing(f64),
}

/// The results of the tests beyond the backtest itself. They aren't saved: they follow from the
/// chart's backtest, and go when it changes.
#[derive(Debug)]
pub struct Tests {
    /// Starts the same every time, so the same inputs give the same results until asked for
    /// another draw.
    pub seed: u64,
    /// The Monte Carlo draws under way, if any.
    pub monte_carlo_run: Option<u64>,
    pub monte_carlo: Option<Result<Arc<robustness::MonteCarlo>, String>>,
    pub blocks: BlockRun,
    pub optimize: OptimizeRun,
    pub walk_forward: TestRun<walk_forward::WalkForward>,
    pub random_entries: TestRun<positions::RandomEntries>,
    pub portfolio: TestRun<portfolio::PortfolioRun>,
    /// The leg whose fills show, and the runs saved to go back to.
    pub portfolio_leg: usize,
    pub saved_portfolios: Vec<portfolio::SavedPortfolio>,
    /// Which of the Portfolio test's views is showing, and what its sweep varies.
    pub portfolio_view: PortfolioView,
    pub portfolio_vary: portfolio_tests::Vary,
    pub portfolio_sweep: TestRun<Vec<portfolio_tests::SweepRow>>,
    pub portfolio_shifted: TestRun<positions::RandomEntries>,
    pub portfolio_random: TestRun<positions::RandomEntries>,
    pub portfolio_monte_carlo: TestRun<portfolio_tests::PortfolioMonteCarlo>,
    pub portfolio_walk: TestRun<walk_forward::WalkForward>,
    /// Random entries moves the backtest's own timing rather than placing its trades afresh.
    pub entries_shifted: bool,
    pub shifted: TestRun<positions::RandomEntries>,
    /// The strategy's parameters before a result was applied, to go back to.
    pub original: Option<Vec<f64>>,
    /// The line picked on a Monte Carlo or Random blocks fan; one past the last for the backtest.
    pub line: Option<usize>,
    /// How much of that fan shows, from 0 to 1; `None` is all of it.
    pub zoom: Option<(f32, f32)>,
    /// Whether Monte Carlo's list shows the picked line's trades, not its notable runs.
    pub showing_trades: bool,
    /// The row of those trades last sent to the chart.
    pub trade_row: Option<usize>,
    /// Monte Carlo as a table of runs rather than lines.
    pub runs_table: bool,
    /// The run picked in that table, and how the table is sorted and scrolled.
    pub run: Option<usize>,
    pub runs_sort: (RunColumn, bool),
    pub runs_scroll: usize,
}

impl Default for Tests {
    fn default() -> Self {
        Self {
            seed: 0,
            monte_carlo_run: None,
            monte_carlo: None,
            blocks: BlockRun::Idle,
            optimize: OptimizeRun::Idle,
            walk_forward: TestRun::Idle,
            random_entries: TestRun::Idle,
            portfolio: TestRun::Idle,
            portfolio_leg: 0,
            saved_portfolios: Vec::new(),
            portfolio_view: PortfolioView::Run,
            portfolio_vary: portfolio_tests::Vary::Scale,
            portfolio_sweep: TestRun::Idle,
            portfolio_shifted: TestRun::Idle,
            portfolio_random: TestRun::Idle,
            portfolio_monte_carlo: TestRun::Idle,
            portfolio_walk: TestRun::Idle,
            entries_shifted: false,
            shifted: TestRun::Idle,
            original: None,
            line: None,
            zoom: None,
            showing_trades: false,
            trade_row: None,
            runs_table: false,
            run: None,
            // Best first.
            runs_sort: (RunColumn::Return, true),
            runs_scroll: 0,
        }
    }
}

/// Where an optimization has got.
#[derive(Debug, Default)]
pub enum OptimizeRun {
    #[default]
    Idle,
    Running {
        run: u64,
        done: usize,
        total: usize,
        stop: StopOnDrop,
    },
    Done(Arc<optimize::OptimizeResult>),
    Failed(String),
}

/// Where a test running as a batch has got.
#[derive(Debug, Default)]
pub enum TestRun<T> {
    #[default]
    Idle,
    Running {
        run: u64,
        done: usize,
        total: usize,
        stop: StopOnDrop,
    },
    Done(Arc<T>),
    Failed(String),
}

impl<T> TestRun<T> {
    /// Moves a running batch on, if `run` is still the one running.
    pub fn progress(&mut self, run: u64, count: usize) {
        if let Self::Running {
            run: running, done, ..
        } = self
            && *running == run
        {
            *done = (*done).max(count);
        }
    }

    /// Takes a batch's result, if `run` is still the one running.
    pub fn finish(&mut self, run: u64, result: Result<Arc<T>, String>) {
        if matches!(self, Self::Running { run: running, .. } if *running == run) {
            *self = match result {
                Ok(found) => Self::Done(found),
                Err(error) => Self::Failed(error),
            };
        }
    }
}

/// Where a batch of Random blocks has got.
#[derive(Debug, Default)]
pub enum BlockRun {
    #[default]
    Idle,
    Running {
        run: u64,
        done: usize,
        total: usize,
        stop: StopOnDrop,
    },
    Done(Arc<robustness::BlockSummary>),
    Failed(String),
}

impl Tests {
    /// Forgets the results and stops what's running, as what they were about has changed.
    pub fn clear(&mut self) {
        self.reset_fan();
        self.clear_monte_carlo();
        self.blocks = BlockRun::Idle;
        self.optimize = OptimizeRun::Idle;
        self.walk_forward = TestRun::Idle;
        self.random_entries = TestRun::Idle;
        self.portfolio = TestRun::Idle;
        self.portfolio_sweep = TestRun::Idle;
        self.portfolio_shifted = TestRun::Idle;
        self.portfolio_random = TestRun::Idle;
        self.portfolio_monte_carlo = TestRun::Idle;
        self.portfolio_walk = TestRun::Idle;
        self.shifted = TestRun::Idle;
        self.original = None;
    }

    /// Scrolls the table of runs so the picked run shows, a little way down it.
    pub fn reveal_run(&mut self, blocks: bool) {
        let Some(run) = self.run else {
            return;
        };
        let mut rows = if blocks {
            match &self.blocks {
                BlockRun::Done(summary) => summary.ranked(),
                _ => return,
            }
        } else {
            match &self.monte_carlo {
                Some(Ok(result)) => result.ranked_runs(),
                _ => return,
            }
        };
        robustness::sort_rows(&mut rows, self.runs_sort);
        if let Some(place) = rows.iter().position(|row| row.run == run) {
            self.runs_scroll = place.saturating_sub(2);
        }
    }

    /// Back to seeing a fan whole, with no line picked.
    pub fn reset_fan(&mut self) {
        self.line = None;
        self.zoom = None;
        self.showing_trades = false;
        self.trade_row = None;
        self.run = None;
        self.runs_scroll = 0;
    }

    pub fn clear_monte_carlo(&mut self) {
        self.monte_carlo_run = None;
        self.monte_carlo = None;
    }
}

/// Stops a batch's workers when it goes, however it goes: replaced, or with the panel.
#[derive(Debug)]
pub struct StopOnDrop(pub Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// A number no earlier test run had, so a late result can't be taken for the current one's.
pub fn next_run() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Panel {
    #[must_use]
    pub fn new(tile: TileId, window_height: f32, settings: &Settings) -> Self {
        Self {
            settings_height: 0.0,
            export_runs: false,
            limits_shown: false,
            decisions_shown: false,
            notice: None,
            plugin_search: plugin_search(settings),
            open_chapters: std::collections::BTreeSet::new(),
            tests: Tests::default(),
            calendar: calendar::CalendarView::default(),
            inspect: None,
            replay_tile: None,
            build: BuildState::default(),
            tile,
            height: (window_height * DEFAULT_SHARE).max(MIN_HEIGHT),
            playback: Playback::default(),
            history: None,
            history_for: None,
            dragging: None,
            timeline: None,
            ticks: None,
            inputs: Vec::new(),
            error: None,
        }
    }

    /// The height it gets in a window this tall: what it was given, but never less than it
    /// takes to show all the settings, unless the window is too short to leave the charts room.
    #[must_use]
    pub fn height_in(&self, window_height: f32) -> f32 {
        let least = MIN_HEIGHT.max(self.settings_height + EDGE_HEIGHT + PANE_FRAME);
        self.height
            .min(window_height * 0.6)
            .max(least)
            .min((window_height - CHARTS_ROOM).max(MIN_HEIGHT.min(window_height * 0.5)))
    }

    /// Starts the plugin search over, offering what `settings` doesn't have on.
    pub fn refresh_plugins(&mut self, settings: &Settings) {
        self.plugin_search = plugin_search(settings);
    }

    /// What's typed in `field`'s box, if anything since it was last applied.
    #[must_use]
    pub fn input(&self, field: Field) -> Option<&str> {
        self.inputs
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, text)| text.as_str())
    }

    pub fn set_input(&mut self, field: Field, text: String) {
        self.inputs.retain(|(f, _)| *f != field);
        self.inputs.push((field, text));
    }
}

fn plugin_search(settings: &Settings) -> combo_box::State<&'static str> {
    combo_box::State::new(
        PLUGIN_SLOTS
            .iter()
            .map(|(slot, _)| *slot)
            .filter(|slot| !settings.plugins.iter().any(|on| on == slot))
            .collect(),
    )
}

/// The second variable's choice for not having one.
pub const NO_VARIABLE: &str = "None";
/// How many bars a replayed trade starts before it opens, to see what led up to it.
pub const REPLAY_LEAD_BARS: usize = 10;

/// What the panel's controls ask for.
#[derive(Clone, Debug)]
pub enum PanelMessage {
    Play,
    Pause,
    /// Back to the span's start.
    Restart,
    /// On to the next bar's close.
    Step,
    Close,
    Speed(f32),
    /// The span's handles moved, and were let go.
    SpanDragged(UnixNanos, UnixNanos),
    SpanReleased,
    /// Test the chart's latest bars instead of a chosen span.
    Latest,
    /// Move the playhead to this time.
    Seek(UnixNanos),
    /// The bottom edge was dragged this far down the window.
    Resize(f32),
    /// The settings measured this tall.
    SettingsHeight(f32),
    /// Backtest this chart with another strategy.
    Strategy(StrategyChoice),
    /// Put a plugin on this chart's backtests, or take it off.
    AddPlugin(String),
    RemovePlugin(String),
    Input(Field, String),
    Submit(Field),
    Edit(Edit),
    /// Show another test.
    Test(TestKind),
    BlockLength(BlockLength),
    /// Run Random blocks, with the same draws as before.
    RunBlocks,
    /// Stop Random blocks.
    StopBlocks,
    /// Run the current test again with new random draws.
    RunAgain,
    /// Move the span back far enough for the test to run.
    WidenSpan,
    /// Change which variable Optimize varies (the first or second), by name; "None" for no second.
    Variable(usize, String),
    ScoreBy(ScoreBy),
    HoldOut(bool),
    RunOptimize,
    StopOptimize,
    RunWalkForward,
    StopWalkForward,
    RunPortfolio,
    StopPortfolio,
    PickWeights,
    LegStrategy(usize, LegChoice),
    PortfolioView(PortfolioView),
    PortfolioVary(VaryChoice),
    EntriesShifted(bool),
    RunShifted,
    StopShifted,
    StopPortfolioTest,
    PortfolioLeg(usize),
    ListPortfolios,
    OpenPortfolio(portfolio::SavedPortfolio),
    RunRandomEntries,
    StopRandomEntries,
    /// Write this chart's setup to a file.
    ExportChart,
    /// Write everything to a file.
    ExportAll,
    /// Whether exporting everything includes full runs.
    ExportRuns(bool),
    /// Read an exported file.
    Import,
    /// Put this result of the optimization on the strategy, by its place in the results.
    Apply(usize),
    /// Back to the strategy's parameters from before any result was applied.
    Revert,
    /// Pick a line on a fan, or none.
    SelectLine(Option<usize>),
    /// Show Monte Carlo as a table of runs, or as lines.
    ShowRunsTable(bool),
    /// Pick a run in the table, or none.
    SelectRun(Option<usize>),
    /// Sort the table of runs by this, or the other way round if it already is.
    SortRuns(RunColumn),
    /// Scroll the table of runs to this row.
    ScrollRuns(usize),
    /// Back to Monte Carlo's notable runs, keeping the line picked.
    ShowRuns,
    /// Move the chart to where this trade really happened: its row, and the bar it ended on.
    ShowTrade(usize, usize),
    /// Show this much of a fan, from 0 to 1, or all of it.
    ZoomFan(Option<(f32, f32)>),
    /// Show the stock settings, or fold them away.
    /// A chapter of the settings opened (true) or closed.
    Chapter(Chapter, bool),
    /// Show the backtest's calendar, or its equity curve.
    ShowCalendar(bool),
    /// Show each limit order rather than the equity curve or calendar.
    ShowLimits(bool),
    ShowDecisions(bool),
    CalendarLevel(calendar::Level),
    /// Move the playhead to just before this trade opened, by its place in the backtest's trades.
    ReplayTrade(usize),
    /// Open a run's trades beside the test, to follow one on the chart.
    Inspect(InspectWhat),
    /// Follow this trade, by its place in the inspection's trades: the playhead goes to just
    /// before it opened, on a chart of its market, for Play to show it happen.
    InspectTrade(usize),
    /// Biggest profit first, or the order they opened.
    InspectByProfit(bool),
    CloseInspection,
    Build(BuildMessage),
}

/// A strategy in the panel's dropdown.
#[derive(Clone, Debug, PartialEq)]
pub struct StrategyChoice {
    pub kind: StrategyKind,
    pub name: String,
}

impl fmt::Display for StrategyChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// Which of the Portfolio test's views is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PortfolioView {
    #[default]
    Run,
    Sweep,
    Shifted,
    RandomEntries,
    MonteCarlo,
    WalkForward,
}

/// What a portfolio's sweep can vary.
#[derive(Clone, Debug, PartialEq)]
pub struct VaryChoice {
    pub label: String,
    pub vary: portfolio_tests::Vary,
}

impl fmt::Display for VaryChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

/// What a portfolio's leg can be told to trade.
#[derive(Clone, Debug, PartialEq)]
pub struct LegChoice {
    pub label: String,
    /// `None` holds its weight, or trades the chart's strategy if that's on.
    pub choice: Option<StrategyChoice>,
}

impl fmt::Display for LegChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

/// A market's whole history, for the span slider.
#[derive(Clone, Debug)]
pub struct History {
    pub first: UnixNanos,
    pub last: UnixNanos,
    /// Coarse closes across it, drawn behind the slider.
    pub closes: Vec<(UnixNanos, f64)>,
}

/// Where playback has got.
#[derive(Clone, Debug)]
pub struct Playback {
    pub playing: bool,
    /// The market time it has reached; `None` shows the whole result.
    pub at: Option<UnixNanos>,
    /// The speed slider's position, from 0 (real time) to 1 (Max).
    pub speed: f32,
    last_frame: Option<Instant>,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            playing: false,
            at: None,
            speed: position_of(10.0),
            last_frame: None,
        }
    }
}

impl Playback {
    /// Starts or resumes from where it is, or from `start` if it isn't anywhere yet or has
    /// finished at `end`.
    pub fn play(&mut self, start: UnixNanos, end: UnixNanos) {
        if self.at.is_none_or(|at| at >= end) {
            self.at = Some(start);
        }
        self.playing = true;
        self.last_frame = None;
    }

    /// Back to not started, keeping the speed.
    pub fn reset(&mut self) {
        *self = Self {
            speed: self.speed,
            ..Self::default()
        };
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.last_frame = None;
    }

    /// Back to the start, paused.
    pub fn restart(&mut self, start: UnixNanos) {
        self.at = Some(start);
        self.pause();
    }

    /// Moves on for the frame at `now`, by the time since the last frame times the speed (at
    /// Max, a whole `start..end` in two seconds); stops at `end`.
    pub fn advance(&mut self, now: Instant, start: UnixNanos, end: UnixNanos) {
        let Some(at) = self.at.filter(|_| self.playing) else {
            return;
        };
        let elapsed = self
            .last_frame
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
        self.last_frame = Some(now);
        let step = match speed_at(self.speed) {
            Some(speed) => elapsed.as_nanos() as f64 * speed,
            None => (end - start) as f64 * elapsed.as_secs_f64() / MAX_SPEED_SECONDS,
        };
        let at = at.saturating_add(step as UnixNanos).min(end);
        self.at = Some(at);
        if at >= end {
            self.pause();
        }
    }
}

/// The speed at slider position `p`, as a multiple of real time; `None` is Max. The left half
/// goes from 1× to 10× for fine control at slow speeds; the right accelerates to 1,000,000×.
#[must_use]
pub fn speed_at(p: f32) -> Option<f64> {
    let p = f64::from(p.clamp(0.0, 1.0));
    if p >= f64::from(MAX_FROM) {
        return None;
    }
    Some(if p <= 0.5 {
        10f64.powf(2.0 * p)
    } else {
        10.0 * 10f64.powf(5.0 * (p - 0.5) / (f64::from(MAX_FROM) - 0.5))
    })
}

/// Where `speed` sits on the slider; [`speed_at`]'s inverse.
#[must_use]
pub fn position_of(speed: f64) -> f32 {
    let speed = speed.max(1.0);
    let p = if speed <= 10.0 {
        speed.log10() / 2.0
    } else {
        0.5 + (speed / 10.0).log10() / 5.0 * (f64::from(MAX_FROM) - 0.5)
    };
    // Just short of Max, so the fastest speed stays a speed.
    (p as f32).min(MAX_FROM - 1e-6)
}

/// The labelled speeds along the slider.
pub const SPEED_LABELS: [(&str, Option<f64>); 13] = [
    ("1×", Some(1.0)),
    ("2", Some(2.0)),
    ("3", Some(3.0)),
    ("5", Some(5.0)),
    ("10", Some(10.0)),
    ("50", Some(50.0)),
    ("100", Some(100.0)),
    ("500", Some(500.0)),
    ("1k", Some(1e3)),
    ("10k", Some(1e4)),
    ("100k", Some(1e5)),
    ("1M", Some(1e6)),
    ("Max", None),
];

/// `×37` and, for bars of `bar_nanos`, how many go by a second: `≈ 2 bars/s`.
#[must_use]
pub fn speed_text(p: f32, bar_nanos: UnixNanos) -> String {
    let Some(speed) = speed_at(p) else {
        return "Max · the rest in 2 s".into();
    };
    let multiple = if speed < 10.0 {
        format!("×{speed:.1}")
    } else {
        format!("×{}", thousands(speed.round() as u64))
    };
    let per_second = speed * 1e9 / bar_nanos.max(1) as f64;
    let rate = if per_second >= 10.0 {
        format!("{} bars/s", thousands(per_second.round() as u64))
    } else if per_second >= 0.1 {
        format!("{per_second:.1} bars/s")
    } else {
        let seconds = 1.0 / per_second;
        if seconds < 120.0 {
            format!("a bar every {seconds:.0} s")
        } else if seconds < 7_200.0 {
            format!("a bar every {:.0} min", seconds / 60.0)
        } else {
            format!("a bar every {:.0} h", seconds / 3_600.0)
        }
    };
    format!("{multiple} · {rate}")
}

/// When each of a chart's bars closes, worked out once per set of bars: time bars a
/// timeframe after they open, tick and volume bars just before the next opens.
#[derive(Debug)]
pub struct Timeline {
    data: Arc<ChartData>,
    pub closes: Vec<UnixNanos>,
}

impl Timeline {
    #[must_use]
    pub fn new(data: &Arc<ChartData>) -> Self {
        Self {
            data: Arc::clone(data),
            closes: data.bars.closes(&data.candles),
        }
    }

    #[must_use]
    pub fn is_for(&self, data: &Arc<ChartData>) -> bool {
        Arc::ptr_eq(&self.data, data)
    }

    /// How many bars have closed by `at`.
    #[must_use]
    pub fn closed_by(&self, at: UnixNanos) -> usize {
        self.closes.partition_point(|&close| close <= at)
    }

    /// When the last bar closes.
    #[must_use]
    pub fn end(&self) -> UnixNanos {
        self.closes.last().copied().unwrap_or(0)
    }

    /// The first bar opening at or after `ts`, the first one traded on.
    #[must_use]
    pub fn first_from(&self, ts: UnixNanos) -> usize {
        self.data.candles.partition_point(|c| c.ts < ts)
    }

    #[must_use]
    pub fn candles(&self) -> &[Candle] {
        &self.data.candles
    }
}

// --- The bar being built ----------------------------------------------------------------------

/// Above this many bars a second, only whole bars show: a forming one would flash by.
pub const FORMING_UP_TO: f64 = 20.0;

/// Where High precision's playback gets the inside of a bar from.
#[derive(Clone, Debug)]
pub enum Source {
    /// The market's ticks, read a day or so at a time.
    Ticks(Arc<dyn TickFeed>),
    /// 1-minute bars across the span.
    Minutes(Arc<Vec<Candle>>),
    /// Nothing finer: a path through the bar's open, low, high and close (high first for a
    /// bar that fell).
    Simulated,
}

/// What [`forming`] reads the inside of a bar from.
#[derive(Clone, Copy, Debug)]
pub enum Inside<'a> {
    Ticks(&'a Ticks),
    Minutes(&'a [Candle]),
    Simulated,
}

/// Ticks read for the playback, covering `from..to`, so each frame doesn't read them again.
#[derive(Debug)]
pub struct TickCache {
    from: UnixNanos,
    to: UnixNanos,
    ticks: Ticks,
}

impl TickCache {
    /// The ticks of `[from, to)`, read whole UTC days at a time when they aren't held yet.
    ///
    /// # Errors
    ///
    /// Returns a message if the ticks can't be read.
    pub fn get<'a>(
        cache: &'a mut Option<Self>,
        feed: &dyn TickFeed,
        from: UnixNanos,
        to: UnixNanos,
    ) -> Result<&'a Ticks, String> {
        const DAY: UnixNanos = persephone_core::time::NANOS_PER_DAY;
        if cache.as_ref().is_none_or(|c| from < c.from || to > c.to) {
            let (start, end) = (from - from % DAY, (to - 1) - (to - 1) % DAY + DAY);
            *cache = Some(Self {
                from: start,
                to: end,
                ticks: feed.ticks(start, end)?,
            });
        }
        Ok(&cache.as_ref().expect("just filled").ticks)
    }
}

/// `bar`, closing at `close`, as it stood at `at`, from `inside`. Before anything has happened
/// inside it, it's flat at its open.
#[must_use]
pub fn forming(bar: &Candle, close: UnixNanos, at: UnixNanos, inside: Inside<'_>) -> Candle {
    let flat = Candle {
        ts: bar.ts,
        high: bar.open,
        low: bar.open,
        close: bar.open,
        volume: 0.0,
        ..*bar
    };
    let built = match inside {
        Inside::Ticks(ticks) => from_ticks(ticks, bar.ts, at),
        Inside::Minutes(minutes) => from_minutes(minutes, bar.ts, at),
        Inside::Simulated => Some(simulated(bar, close, at)),
    };
    built.map_or(flat, |c| Candle {
        ts: bar.ts,
        open: bar.open,
        high: c.high.max(bar.open),
        low: c.low.min(bar.open),
        ..c
    })
}

fn aggregate(points: impl Iterator<Item = (f64, f64, f64, f64)>) -> Option<Candle> {
    points.fold(None, |acc: Option<Candle>, (open, high, low, close)| {
        Some(match acc {
            None => Candle {
                ts: 0,
                open,
                high,
                low,
                close,
                volume: 0.0,
            },
            Some(c) => Candle {
                high: c.high.max(high),
                low: c.low.min(low),
                close,
                ..c
            },
        })
    })
}

/// The minutes of `[open, at)` that have closed, as one bar.
fn from_minutes(minutes: &[Candle], open: UnixNanos, at: UnixNanos) -> Option<Candle> {
    let minute = Timeframe::M1.nanos();
    let first = minutes.partition_point(|m| m.ts < open);
    let done = &minutes[first..];
    let done = &done[..done.partition_point(|m| m.ts + minute <= at)];
    let volume = done.iter().map(|m| m.volume).sum();
    aggregate(done.iter().map(|m| (m.open, m.high, m.low, m.close))).map(|c| Candle { volume, ..c })
}

/// The ticks of `[open, at)` as one bar: trade prices, or the middle of the quotes.
fn from_ticks(ticks: &Ticks, open: UnixNanos, at: UnixNanos) -> Option<Candle> {
    match ticks {
        Ticks::Trades(trades) => {
            let first = trades.partition_point(|t| t.ts < open);
            let done = &trades[first..];
            let done = &done[..done.partition_point(|t| t.ts < at)];
            let volume = done.iter().map(|t| t.size).sum();
            aggregate(done.iter().map(|t| (t.price, t.price, t.price, t.price)))
                .map(|c| Candle { volume, ..c })
        }
        Ticks::Quotes(quotes) => {
            let first = quotes.partition_point(|q| q.ts < open);
            let done = &quotes[first..];
            let done = &done[..done.partition_point(|q| q.ts < at)];
            aggregate(done.iter().map(|q| {
                let mid = q.mid();
                (mid, mid, mid, mid)
            }))
            .map(|c| Candle {
                volume: done.len() as f64,
                ..c
            })
        }
    }
}

/// The bar at `at` along a path open → low → high → close (open → high → low → close for a bar
/// that fell), each leg taking a third of it.
fn simulated(bar: &Candle, close: UnixNanos, at: UnixNanos) -> Candle {
    let span = close.saturating_sub(bar.ts).max(1) as f64;
    let share = (at.saturating_sub(bar.ts) as f64 / span).clamp(0.0, 1.0);
    let path = if bar.close >= bar.open {
        [bar.open, bar.low, bar.high, bar.close]
    } else {
        [bar.open, bar.high, bar.low, bar.close]
    };
    let leg = ((share * 3.0) as usize).min(2);
    let within = share * 3.0 - leg as f64;
    let price = path[leg] + (path[leg + 1] - path[leg]) * within;
    let visited = path[..=leg].iter().copied().chain(std::iter::once(price));
    let (low, high) = visited.fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p), hi.max(p)));
    Candle {
        ts: bar.ts,
        open: bar.open,
        high,
        low,
        close: price,
        volume: bar.volume * share,
    }
}

/// The backtest as of the playhead: with `bars` bars closed, of which the first `from` only
/// warmed the strategy up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    pub equity: f64,
    pub return_: f64,
    /// Deepest fall from a peak so far, as a fraction (negative).
    pub drawdown: f64,
    pub trades: usize,
}

#[must_use]
pub fn progress(report: &BacktestReport, from: usize, bars: usize) -> Progress {
    let start = report.starting_balance;
    let upto = bars.min(report.equity.len());
    let equity = &report.equity[from.min(upto)..upto];
    let mut peak = start;
    let mut drawdown: f64 = 0.0;
    for &value in equity {
        peak = peak.max(value);
        if peak > 0.0 {
            drawdown = drawdown.min(value / peak - 1.0);
        }
    }
    let now = equity.last().copied().unwrap_or(start);
    Progress {
        equity: now,
        return_: if start > 0.0 { now / start - 1.0 } else { 0.0 },
        drawdown,
        trades: report.fills.iter().filter(|f| f.bar < bars).count(),
    }
}

#[cfg(test)]
mod tests {
    use persephone_core::Timeframe;

    use super::*;

    #[test]
    fn the_speed_slider_is_fine_on_the_left_and_fast_on_the_right() {
        assert_eq!(speed_at(0.0), Some(1.0));
        assert!((speed_at(0.5).unwrap() - 10.0).abs() < 1e-9);
        assert!((speed_at(MAX_FROM - 1e-6).unwrap() - 1e6).abs() / 1e6 < 1e-3);
        assert_eq!(speed_at(1.0), None, "Max");
        let mut last = 0.0;
        for i in 0..950 {
            let speed = speed_at(i as f32 / 1_000.0).unwrap();
            assert!(speed > last, "rises at {i}");
            last = speed;
        }
        for (_, speed) in SPEED_LABELS {
            let Some(speed) = speed else { continue };
            let back = speed_at(position_of(speed)).unwrap();
            assert!((back - speed).abs() / speed < 1e-3, "{speed} → {back}");
        }
        assert!(position_of(5.0) < 0.5 && position_of(50.0) > 0.5);
    }

    #[test]
    fn speeds_read_as_multiples_and_bar_rates() {
        let minute = Timeframe::M1.nanos();
        assert_eq!(speed_text(0.0, minute), "×1.0 · a bar every 60 s");
        assert_eq!(speed_text(position_of(120.0), minute), "×120 · 2.0 bars/s");
        assert_eq!(
            speed_text(position_of(60_000.0), minute),
            "×60,000 · 1,000 bars/s"
        );
        assert_eq!(speed_text(1.0, minute), "Max · the rest in 2 s");
    }

    #[test]
    fn playback_moves_with_time_and_speed_and_stops_at_the_end() {
        let now = Instant::now();
        let second = Duration::from_secs(1);
        let mut playback = Playback {
            speed: position_of(60.0),
            ..Playback::default()
        };
        playback.play(1_000, 1_000_000_000_000);
        playback.advance(now, 1_000, 1_000_000_000_000);
        assert_eq!(
            playback.at,
            Some(1_000),
            "the first frame only starts the clock"
        );
        playback.advance(now + second, 1_000, 1_000_000_000_000);
        let at = playback.at.unwrap();
        assert!(
            (at as f64 - 60e9 - 1e3).abs() < 1e7,
            "a minute of market time: {at}"
        );

        playback.pause();
        playback.advance(now + 10 * second, 1_000, 1_000_000_000_000);
        assert_eq!(playback.at, Some(at), "paused");

        playback.speed = 1.0;
        playback.play(1_000, 1_000_000_000_000);
        playback.advance(now, 1_000, 1_000_000_000_000);
        playback.advance(now + second, 1_000, 1_000_000_000_000);
        assert!(playback.playing, "Max takes two seconds");
        playback.advance(now + 3 * second, 1_000, 1_000_000_000_000);
        assert_eq!(playback.at, Some(1_000_000_000_000));
        assert!(!playback.playing, "stopped at the end");

        playback.play(1_000, 1_000_000_000_000);
        assert_eq!(playback.at, Some(1_000), "playing again starts over");
        playback.restart(5);
        assert_eq!((playback.at, playback.playing), (Some(5), false));
    }

    #[test]
    fn bars_close_on_their_timeframe_or_when_the_next_opens() {
        let hour = Timeframe::H1.nanos();
        let candles: Vec<Candle> = (0..5)
            .map(|i| Candle {
                ts: i * hour,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            })
            .collect();
        let hourly = Timeline::new(&ChartData::new(
            "T",
            "SIM",
            Timeframe::H1,
            2,
            candles.clone(),
        ));
        assert_eq!(hourly.closed_by(hour - 1), 0);
        assert_eq!(hourly.closed_by(hour), 1);
        assert_eq!(hourly.closed_by(hourly.end()), 5);
        assert_eq!(hourly.first_from(hour + 1), 2);
        let ticks = Timeline::new(&ChartData::new(
            "T",
            "SIM",
            persephone_core::BarSpec::Ticks(100),
            2,
            candles,
        ));
        assert_eq!(
            ticks.closed_by(2 * hour + 5),
            2,
            "a tick bar closes as the next opens"
        );
        assert_eq!(ticks.closed_by(ticks.end()), 5);
    }

    fn candle(ts: UnixNanos, open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            ts,
            open,
            high,
            low,
            close,
            volume: 10.0,
        }
    }

    #[test]
    fn forming_bars_grow_from_what_happened_inside_them() {
        let hour = Timeframe::H1.nanos();
        let minute = Timeframe::M1.nanos();
        let bar = candle(hour, 100.0, 110.0, 90.0, 105.0);
        // Minutes inside the hour: up to 104, down to 95.
        let minutes: Vec<Candle> = (0..60)
            .map(|m| {
                let p = 100.0 + 4.0 * ((m as f64) / 10.0).sin();
                candle(hour + m * minute, p, p + 1.0, p - 1.0, p)
            })
            .collect();
        let minutes = Inside::Minutes(&minutes);
        let at_open = forming(&bar, 2 * hour, hour, minutes);
        assert_eq!(
            (at_open.high, at_open.low, at_open.close),
            (100.0, 100.0, 100.0),
            "flat"
        );
        let quarter = forming(&bar, 2 * hour, hour + 15 * minute, minutes);
        assert!(quarter.high > 104.0 && quarter.high < 106.0, "{quarter:?}");
        assert!(quarter.volume == 150.0, "fifteen minutes");
        assert_eq!(quarter.ts, hour);

        // Without finer data: open → low → high → close over the hour.
        let path = |share: f64| {
            forming(
                &bar,
                2 * hour,
                hour + (share * hour as f64) as u64,
                Inside::Simulated,
            )
        };
        assert_eq!(path(1.0 / 3.0).low, 90.0, "the low after a third");
        assert_eq!(path(1.0 / 3.0).high, 100.0);
        assert_eq!(path(2.0 / 3.0).high, 110.0);
        let done = path(1.0);
        assert_eq!(
            (done.open, done.high, done.low, done.close),
            (100.0, 110.0, 90.0, 105.0)
        );
        let fell = candle(0, 100.0, 110.0, 90.0, 95.0);
        assert_eq!(
            forming(&fell, hour, hour / 3, Inside::Simulated).high,
            110.0,
            "high first"
        );

        let trades = Ticks::Trades(
            (0..10)
                .map(|i| persephone_core::tick::Trade {
                    ts: hour + i * minute,
                    price: 100.0 + i as f64,
                    size: 2.0,
                    aggressor: persephone_core::tick::Aggressor::Unknown,
                })
                .collect(),
        );
        let ticked = forming(&bar, 2 * hour, hour + 5 * minute, Inside::Ticks(&trades));
        assert_eq!(
            (ticked.high, ticked.close, ticked.volume),
            (104.0, 104.0, 10.0)
        );
    }

    #[test]
    fn progress_counts_only_what_has_happened() {
        let report = BacktestReport {
            equity: vec![100.0, 100.0, 110.0, 99.0, 120.0],
            starting_balance: 100.0,
            fills: vec![
                persephone_engine::Fill {
                    ts: 0,
                    bar: 1,
                    side: persephone_core::Side::Buy,
                    price: 1.0,
                    qty: 1.0,
                    fee: 0.0,
                    maker: false,
                    range_slip: 0.0,
                },
                persephone_engine::Fill {
                    ts: 0,
                    bar: 4,
                    side: persephone_core::Side::Sell,
                    price: 1.0,
                    qty: 1.0,
                    fee: 0.0,
                    maker: false,
                    range_slip: 0.0,
                },
            ],
            ..BacktestReport::default()
        };
        let p = progress(&report, 1, 4);
        assert_eq!((p.equity, p.trades), (99.0, 1));
        assert!((p.return_ + 0.01).abs() < 1e-12);
        assert!((p.drawdown - (99.0 / 110.0 - 1.0)).abs() < 1e-12);
        assert_eq!(progress(&report, 1, 5).trades, 2);
    }
}
