//! The panel and its settings sheet, laid out.

use iced::{
    Alignment, Background, Border, Color, Element, Fill, FillPortion, Padding, overlay,
    widget::{
        Space, button, canvas, checkbox, column, combo_box, container, pick_list, row, scrollable,
        sensor, stack, text, text_input, toggler, tooltip,
    },
};
use persephone_chart::{PRICE_AXIS_WIDTH, theme};
use persephone_core::{BarSpec, Candle, Side, UnixNanos, strategy::StrategyKind, time::DateTime};
use persephone_engine::{BacktestReport, LimitOutcome, PortfolioReport};
use persephone_research::figures::LimitSummary;

use super::{
    BlockLength, BlockRun, Commission, Edit, Field, FillTiming, InspectWhat, LegChoice,
    NO_VARIABLE, OneWay, OptimizeRun, Panel, PanelMessage, PortfolioView, Precision, RunColumn,
    ScoreBy, Settings, StrategyChoice, TestKind, TestRun, VaryChoice,
    calendar::{Calendar, Level, Total, days_in_month, weeks},
    optimize::{OptimizeResult, Outcome},
    portfolio::{PortfolioRun, SavedPortfolio},
    portfolio_tests::{PortfolioMonteCarlo, SweepRow, Vary},
    positions::ONE_TRADE,
    positions::RandomEntries,
    progress,
    robustness::Problem,
    robustness::percentile,
    robustness::{
        BlockStats, BlockSummary, LineStats, MonteCarlo, RoundTrip, RunRow, RunSummary,
        round_trips, sort_rows, summarize_run,
    },
    speed_text,
    walk_forward::WalkForward,
    widgets::heat,
    widgets::{
        BarsByTime, Distribution, Edge, Equity, Fan, Glyph, HeatScale, Icon, Landscape, Lines,
        RunsTable, Span, Speed, WalkChart, leg_color, percent,
    },
};

mod build;
mod calendar;
mod inspect;
mod results;
mod settings;

pub use settings::test_tabs;

use self::{build::*, calendar::*, inspect::*, results::*, settings::*};

/// The settings' share of the panel's width, in tenths.
const SETTINGS_PORTION: u16 = 3;
/// Text in the settings section, the sizes of the right-click menu's labels and hints.
const SMALL: f32 = 12.0;
const HINT: f32 = 11.0;
/// The rows under the equity pane.
const SPEED_HEIGHT: f32 = 30.0;
const SPAN_HEIGHT: f32 = 26.0;
/// How wide an icon button is, the transport's column and the figures beside the sliders.
const ICON_WIDTH: f32 = 26.0;
const LEFT_WIDTH: f32 = 3.0 * ICON_WIDTH + 4.0;
const FIGURES_WIDTH: f32 = 180.0;
/// The Optimize test's list of the best.
const RESULTS_WIDTH: f32 = 250.0;
const RESULT_FIGURE: f32 = 56.0;
/// Walk-forward's table of windows, beside its chart.
const WALK_TABLE_WIDTH: f32 = 494.0;
/// How far down the legend over the pane reaches.
const WALK_TOP: f32 = 48.0;
/// A value that made money where it was picked and lost it on bars it hadn't seen.
const FELL_APART: Color = Color::from_rgb8(0xff, 0xa7, 0x26);
/// The legend's figures, the size of a chart's O/H/L/C.
const LEGEND_FIGURES: f32 = theme::TEXT_SIZE + 1.0;

const PANEL: Color = Color::from_rgb8(0x1e, 0x22, 0x2d);
const PANEL_BORDER: Color = Color::from_rgb8(0x36, 0x3a, 0x45);
const HOVER: Color = Color::from_rgb8(0x2a, 0x2e, 0x39);

/// Where the chart's backtest stands.
pub enum Status<'a> {
    /// The chart has no strategy yet.
    NoStrategy,
    Waiting,
    Running,
    Failed(&'a str),
    Done(&'a BacktestReport),
}

/// What the panel shows, gathered by the app.
pub struct PanelInfo<'a> {
    pub panel: &'a Panel,
    pub settings: &'a Settings,
    /// `BTCUSDT · 1h · EMA Cross`.
    pub title: String,
    pub status: Status<'a>,
    pub quote: &'a str,
    /// The span chosen, or `None` for the chart's latest bars.
    pub span: Option<(UnixNanos, UnixNanos)>,
    /// The bars traded on: the first one's open and the last one's close.
    pub traded: Option<(UnixNanos, UnixNanos)>,
    /// Index of the first bar traded on (earlier ones only warm the strategy up).
    pub from_index: usize,
    /// The bars traded on, and how they're cut.
    pub candles: &'a [Candle],
    pub bars: BarSpec,
    /// Bars closed at the playhead, while there is one.
    pub shown: Option<usize>,
    /// A bar's usual length, for the speed's bars-per-second.
    pub bar_nanos: UnixNanos,
    /// How orders fill, when worth saying.
    pub fills: Option<String>,
    /// How the finished run compares: with no costs, and with the market.
    pub summary: Option<String>,
    /// The share of the capital a position is worth, for showing the market at the same size.
    pub market_share: f64,
    /// The strategies to pick from, and the chart's own.
    pub strategies: Vec<StrategyChoice>,
    pub strategy: Option<StrategyChoice>,
    /// The panel's width.
    pub width: f32,
    /// Why the chosen test can't run right now.
    pub problem: Option<Problem>,
    /// What an optimization would come to, if its settings make sense.
    pub plan: Option<String>,
    /// The strategy's current values, and its parameters' names.
    pub params: &'a [f64],
    pub variables: Vec<String>,
    /// On the Build tab: the chart's tree, and whether it's what the chart is running.
    pub draft: Option<(super::logic_draft::Draft, bool)>,
}

pub fn panel(info: PanelInfo<'_>) -> Element<'_, PanelMessage> {
    let panel = info.panel;
    let settings = settings_section(&info);
    let test = info.settings.test;
    // The backtest plays back, on its own tab or while its tree is built.
    // In any other test, a trade picked in its drawer plays its run instead.
    let trade_picked = !matches!(test, TestKind::Backtest | TestKind::Build)
        && panel
            .inspect
            .as_ref()
            .is_some_and(|inspection| inspection.picked.is_some());
    let ready = (matches!(test, TestKind::Backtest | TestKind::Build)
        && matches!(info.status, Status::Done(_)))
        || trade_picked;
    let started = panel.playback.at.is_some();

    let play = if panel.playback.playing {
        icon_button(
            Glyph::Pause,
            "Pause the playback",
            None,
            Some(PanelMessage::Pause),
            true,
        )
    } else {
        let what = if trade_picked {
            "Play this run from the trade picked, bar by bar, on its chart"
        } else if started {
            "Carry on playing the backtest"
        } else {
            "Play the backtest back bar by bar, on the chart too"
        };
        let play = ready.then_some(PanelMessage::Play);
        icon_button(Glyph::Play, what, None, play, true)
    };
    let transport = row![
        play,
        icon_button(
            Glyph::Restart,
            "Back to the start of the span, paused",
            None,
            (ready && started).then_some(PanelMessage::Restart),
            false,
        ),
        icon_button(
            Glyph::Step,
            "On to the next bar's close, paused",
            None,
            ready.then_some(PanelMessage::Step),
            false,
        ),
    ]
    .spacing(2)
    .width(LEFT_WIDTH);

    // The legend, over the pane's top left as on a chart.
    let muted = |content: String| {
        text(content)
            .size(theme::TEXT_SIZE)
            .color(theme::TEXT_MUTED)
    };
    // A portfolio isn't this chart's market or strategy.
    let title = if test == TestKind::Portfolio {
        format!("Portfolio · {} markets", info.settings.portfolio.legs.len())
    } else {
        info.title.clone()
    };
    let title = text(title)
        .size(theme::LEGEND_SIZE)
        .color(theme::TEXT_BRIGHT);
    let mut heading = row![title].spacing(14).align_y(Alignment::Center);
    let mut note = None;
    let mut crumbs: Option<Element<'_, PanelMessage>> = None;
    let figure = |name: &'static str, value: String, color: Color, what: &'static str| {
        tip(
            row![
                text(name)
                    .size(LEGEND_FIGURES)
                    .font(iced::Font::MONOSPACE)
                    .color(theme::TEXT_MUTED),
                text(value)
                    .size(LEGEND_FIGURES)
                    .font(iced::Font::MONOSPACE)
                    .color(color),
            ]
            .spacing(6),
            what,
            None,
        )
    };
    let up_or_down = |value: f64| if value >= 0.0 { theme::UP } else { theme::DOWN };
    let pane: Element<'_, PanelMessage> = match (&info.status, test) {
        (_, TestKind::Build) => match &info.draft {
            Some((draft, on_chart)) => build_pane(panel, draft, *on_chart),
            None => Space::new().width(Fill).height(Fill).into(),
        },
        (Status::Done(report), TestKind::Backtest) => {
            let bars = info.shown.unwrap_or(report.equity.len());
            let now = progress(report, info.from_index, bars);
            let change = up_or_down(now.return_);
            heading = heading
                .push(figure(
                    "Return",
                    format!("{:+.2}%", now.return_ * 100.0),
                    change,
                    "How much the capital has grown or shrunk by this point, open position included.",
                ))
                .push(figure(
                    "DD",
                    format!("{:.2}%", now.drawdown * 100.0),
                    change,
                    "Drawdown: the deepest fall from a peak in equity so far.",
                ))
                .push(figure(
                    "Fills",
                    now.trades.to_string(),
                    theme::TEXT,
                    "How many orders have filled so far.",
                ))
                .push(figure(
                    "Equity",
                    format!("{:.2} {}", now.equity, info.quote),
                    theme::TEXT,
                    "What the account is worth by this point: cash plus the open position at the last close.",
                ));
            if note.is_none() {
                note = info.summary.clone().map(muted);
            }
            let view = panel.calendar;
            heading = heading.push(
                row![
                    tip(
                        button(text("Equity").size(HINT).center())
                            .padding([2, 7])
                            .on_press(PanelMessage::ShowCalendar(false))
                            .style(move |_, status| chip(!view.shown && !panel.limits_shown, status)),
                        "The account's value over the span.",
                        None,
                    ),
                    tip(
                        button(text("Calendar").size(HINT).center())
                            .padding([2, 7])
                            .on_press(PanelMessage::ShowCalendar(true))
                            .style(move |_, status| chip(view.shown, status)),
                        "What each month and day made. Click down to a day's trades and replay them on the chart.",
                        None,
                    ),
                ]
                .spacing(2),
            );
            let inspecting = panel.inspect.is_some();
            heading = heading.push(tip(
                button(text("Trades").size(HINT).center())
                    .padding([2, 7])
                    .on_press(if inspecting {
                        PanelMessage::CloseInspection
                    } else {
                        PanelMessage::Inspect(InspectWhat::Backtest)
                    })
                    .style(move |_, status| chip(inspecting, status)),
                "Every trade, beside the chart: click one to put the playhead just before it opened, and Play to watch it.",
                None,
            ));
            if !report.limit_log.is_empty() {
                let shown = panel.limits_shown;
                heading = heading.push(tip(
                    button(text("Limits").size(HINT).center())
                        .padding([2, 7])
                        .on_press(PanelMessage::ShowLimits(!shown))
                        .style(move |_, status| chip(shown, status)),
                    "Every limit order: what became of it, how far past its price the market traded, and how far the market then moved its way.",
                    None,
                ));
            }
            if !report.decisions.is_empty() {
                let shown = panel.decisions_shown;
                heading = heading.push(tip(
                    button(text("Decisions").size(HINT).center())
                        .padding([2, 7])
                        .on_press(PanelMessage::ShowDecisions(!shown))
                        .style(move |_, status| chip(shown, status)),
                    "Each bar's decision: what the strategy asked for, what was held, what went out, and why not.",
                    None,
                ));
            }
            if panel.decisions_shown && !report.decisions.is_empty() {
                decisions_table(report)
            } else if panel.limits_shown && !report.limit_log.is_empty() {
                limit_orders_table(report)
            } else if view.shown {
                let last = info.candles.last().map_or(0, |c| c.ts);
                let calendar = Calendar::new(round_trips(report), last);
                crumbs = Some(calendar_crumbs(view.level));
                calendar_pane(
                    &calendar,
                    view.level,
                    view.replaying,
                    report.starting_balance,
                )
            } else {
                canvas(Equity {
                    values: report.equity.get(info.from_index..).unwrap_or_default(),
                    candles: info.candles,
                    bars: info.bars,
                    bar_nanos: info.bar_nanos,
                    shown: bars.saturating_sub(info.from_index),
                    start: report.starting_balance,
                    market_share: info.market_share,
                })
                .width(Fill)
                .height(Fill)
                .into()
            }
        }
        (Status::Done(report), TestKind::MonteCarlo) => match &panel.tests.monte_carlo {
            Some(Ok(result)) => {
                heading = heading
                    .push(figure(
                        "Median",
                        percent(result.median),
                        up_or_down(result.median),
                        "The return half the runs beat, drawing the backtest's trades at random.",
                    ))
                    .push(figure(
                        "5–95%",
                        format!("{} to {}", percent(result.low), percent(result.high)),
                        theme::TEXT,
                        "Nine runs in ten ended between these returns. A low end below zero means these trades can lose money overall.",
                    ))
                    .push(figure(
                        "DD 95%",
                        format!("{:.1}%", result.drawdown * 100.0),
                        theme::TEXT,
                        "Only one run in twenty fell further than this from a peak: a drawdown to be ready for.",
                    ))
                    .push(figure(
                        "Ruin",
                        format!("{:.1}%", result.ruin * 100.0),
                        if result.ruin > 0.0 {
                            theme::DOWN
                        } else {
                            theme::TEXT
                        },
                        "The share of runs that fell as far as the Ruin at setting from a peak.",
                    ));
                note = Some(muted(format!(
                    "Each line is one possible history: {} of the backtest's trades drawn at random. Click one, or pick from the list. Scroll to zoom.",
                    result.trades
                )));
                heading = heading.push(runs_view_chips(panel.tests.runs_table));
                if panel.tests.runs_table {
                    note = None;
                    runs_pane(
                        result,
                        panel.tests.run,
                        panel.tests.runs_sort,
                        panel.tests.runs_scroll,
                        &round_trips(report),
                        report.starting_balance,
                        panel.tests.trade_row,
                    )
                } else {
                    let mut stats = result.stats.clone();
                    stats.push(result.actual_stats);
                    row![
                        canvas(Fan {
                            paths: &result.paths,
                            median: &result.median_path,
                            actual: Some(&result.actual),
                            stats,
                            marks: vec![(result.best, theme::UP), (result.worst, theme::DOWN)],
                            picked: panel.tests.line,
                            zoom: panel.tests.zoom.unwrap_or((0.0, 1.0)),
                            start: report.starting_balance,
                            unit: "Trade",
                            stride: result.stride,
                            line_name: "This run",
                        })
                        .width(FillPortion(1))
                        .height(Fill),
                        match panel.tests.line.filter(|_| panel.tests.showing_trades) {
                            Some(line) => run_trades(
                                result,
                                line,
                                &round_trips(report),
                                report.starting_balance,
                                panel.tests.trade_row,
                            ),
                            None => monte_carlo_list(
                                result,
                                panel.tests.line,
                                panel.tests.zoom.is_some(),
                            ),
                        },
                    ]
                    .into()
                }
            }
            // Said under the test's settings.
            Some(Err(_)) => Space::new().width(Fill).height(Fill).into(),
            None => {
                note = Some(muted("Drawing the trades…".into()));
                Space::new().width(Fill).height(Fill).into()
            }
        },
        (_, TestKind::Portfolio) if panel.tests.portfolio_view == PortfolioView::Sweep => {
            match &panel.tests.portfolio_sweep {
                TestRun::Done(rows) => {
                    let name = panel.tests.portfolio_vary.name(&info.settings.portfolio);
                    note = Some(muted(format!(
                        "{} values of {name}, best first.",
                        rows.len()
                    )));
                    portfolio_sweep_table(rows)
                }
                TestRun::Running { done, total, .. } => {
                    note = Some(muted(format!("Running {done} of {total}…")));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Failed(message) => {
                    note = Some(text(message.as_str()).size(SMALL).color(theme::DOWN));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Idle => {
                    note = Some(muted(
                        "Pick what to vary and its range, then press Run.".to_owned(),
                    ));
                    Space::new().width(Fill).height(Fill).into()
                }
            }
        }
        (_, TestKind::Portfolio) if panel.tests.portfolio_view == PortfolioView::Shifted => {
            match &panel.tests.portfolio_shifted {
                TestRun::Done(found) => {
                    heading = heading
                        .push(figure("This book", percent(found.actual_return), up_or_down(found.actual_return), "This book as a table of weights, unmoved: what the moved ones are held against."))
                        .push(figure("Median", percent(found.median), up_or_down(found.median), "The return half the moved ones beat."))
                        .push(figure("As good", format!("{:.0}%", found.share_matching_return * 100.0), theme::TEXT, "The share of moved ones that returned at least as much. Near 50% means the timing added nothing."))
                        .push(figure("As safe", format!("{:.0}%", found.share_matching_fall * 100.0), theme::TEXT, "The share of moved ones that fell no further."));
                    note = Some(muted(if found.trades < 2 {
                        SHIFTED_ONE.to_owned()
                    } else {
                        format!(
                            "{} changes moved along the span {} times. 19 in 20 returned {} to {}.",
                            found.trades,
                            found.runs,
                            percent(found.low),
                            percent(found.high),
                        )
                    }));
                    shifted_charts(found)
                }
                TestRun::Running { done, total, .. } => {
                    note = Some(muted(format!("Running {done} of {total}…")));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Failed(message) => {
                    note = Some(text(message.as_str()).size(SMALL).color(theme::DOWN));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Idle => {
                    note = Some(muted(
                        "Press Run to move the last run's book along the span.".to_owned(),
                    ));
                    Space::new().width(Fill).height(Fill).into()
                }
            }
        }
        (_, TestKind::Portfolio) if panel.tests.portfolio_view == PortfolioView::RandomEntries => {
            match &panel.tests.portfolio_random {
                TestRun::Done(found) => {
                    heading = heading
                        .push(figure("This book", percent(found.actual_return), up_or_down(found.actual_return), "The run's own trades as a table of weights at their own times: what the placings are held against."))
                        .push(figure("Median", percent(found.median), up_or_down(found.median), "The return half the placings beat."))
                        .push(figure("As good", format!("{:.0}%", found.share_matching_return * 100.0), theme::TEXT, "The share of placings that returned at least as much. Near 50% means the entries added nothing."))
                        .push(figure("As safe", format!("{:.0}%", found.share_matching_fall * 100.0), theme::TEXT, "The share of placings that fell no further."));
                    note = Some(muted(if found.trades < 2 {
                        "With one trade there's little to place at random, so this says almost nothing.".to_owned()
                    } else {
                        format!(
                            "{} trades placed at random {} times, each leg's apart. 19 in 20 returned {} to {}. Click the returns to see a placing's trades.",
                            found.trades,
                            found.runs,
                            percent(found.low),
                            percent(found.high),
                        )
                    }));
                    placing_charts(found, |value| {
                        PanelMessage::Inspect(InspectWhat::PortfolioRandomPlacing(value))
                    })
                }
                TestRun::Running { done, total, .. } => {
                    note = Some(muted(format!("Running {done} of {total}…")));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Failed(message) => {
                    note = Some(text(message.as_str()).size(SMALL).color(theme::DOWN));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Idle => {
                    note = Some(muted(
                        "Press Run to place the last run's trades at random along the span."
                            .to_owned(),
                    ));
                    Space::new().width(Fill).height(Fill).into()
                }
            }
        }
        (_, TestKind::Portfolio) if panel.tests.portfolio_view == PortfolioView::MonteCarlo => {
            match &panel.tests.portfolio_monte_carlo {
                TestRun::Done(found) => {
                    heading = heading
                        .push(figure(
                            "This run",
                            percent(found.actual_return),
                            up_or_down(found.actual_return),
                            "What the run itself returned.",
                        ))
                        .push(figure(
                            "Median",
                            percent(found.median),
                            up_or_down(found.median),
                            "The return half the draws beat.",
                        ))
                        .push(figure(
                            "Losing",
                            format!("{:.0}%", found.losing_share * 100.0),
                            theme::TEXT,
                            "The share of draws that lost money.",
                        ))
                        .push(figure(
                            "Ruin",
                            format!("{:.0}%", found.ruin_share * 100.0),
                            theme::TEXT,
                            "The share of draws that fell past the ruin level.",
                        ));
                    note = Some(muted(format!(
                        "Its returns drawn {} times in blocks of {} bars. 19 in 20 returned {} to {}.",
                        found.runs,
                        found.block_bars,
                        percent(found.low),
                        percent(found.high),
                    )));
                    row![
                        container(with_legend(portfolio_band(found, info.settings.capital)))
                            .padding(Padding {
                                top: WALK_TOP,
                                ..Padding::ZERO
                            })
                            .width(FillPortion(3))
                            .height(Fill),
                        canvas(Distribution {
                            title: "How far each fell at worst",
                            values: &found.falls,
                            actual: found.actual_fell,
                            band: None,
                            median: percentile(&found.falls, 0.5),
                            pick: None,
                        })
                        .width(FillPortion(2))
                        .height(Fill),
                    ]
                    .into()
                }
                TestRun::Failed(message) => {
                    note = Some(text(message.as_str()).size(SMALL).color(theme::DOWN));
                    Space::new().width(Fill).height(Fill).into()
                }
                _ => {
                    note = Some(muted(
                        "Press Run to draw the last run's returns again.".to_owned(),
                    ));
                    Space::new().width(Fill).height(Fill).into()
                }
            }
        }
        (_, TestKind::Portfolio) if panel.tests.portfolio_view == PortfolioView::WalkForward => {
            match &panel.tests.portfolio_walk {
                TestRun::Done(found) => {
                    heading = heading
                        .push(figure("Unseen", percent(found.unseen_return), up_or_down(found.unseen_return), "What the unseen stretches returned, added up. None of it was used to pick."))
                        .push(figure("Equal shares", percent(found.buy_and_hold_return), up_or_down(found.buy_and_hold_return), "Equal shares of the markets over the same stretches, added up, with no funding taken off."))
                        .push(figure("Profitable", format!("{} of {}", found.profitable_windows, found.windows.len()), theme::TEXT, "How many unseen stretches made money."))
                        .push(figure("Worst fall", format!("{:.1}%", found.deepest_fall * 100.0), theme::TEXT, "The deepest fall from a peak in any unseen stretch."));
                    note = Some(muted(format!(
                        "Each window picks on {} bars, then is tested on the next {}. Each starts from the same capital. One after another, that's {} against {}.",
                        found.train_bars,
                        found.test_bars,
                        percent(found.compounded().0),
                        percent(found.compounded().1),
                    )));
                    row![
                        container(
                            canvas(WalkChart {
                                found,
                                market: "Market, held in full"
                            })
                            .width(Fill)
                            .height(Fill)
                        )
                        .padding(Padding {
                            top: WALK_TOP,
                            ..Padding::ZERO
                        })
                        .width(Fill)
                        .height(Fill),
                        walk_forward_table(
                            found,
                            "Equal",
                            InspectWhat::PortfolioWalkWindow,
                            inspected_window(panel, true),
                        ),
                    ]
                    .into()
                }
                TestRun::Running { done, total, .. } => {
                    note = Some(muted(format!("Running {done} of {total}…")));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Failed(message) => {
                    note = Some(text(message.as_str()).size(SMALL).color(theme::DOWN));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Idle => {
                    note = Some(muted(
                        "Pick what to vary and its range, then press Run.".to_owned(),
                    ));
                    Space::new().width(Fill).height(Fill).into()
                }
            }
        }
        (_, TestKind::Portfolio) => match &panel.tests.portfolio {
            TestRun::Done(found) => {
                let figures = &found.figures;
                let report = &found.report;
                heading = heading
                    .push(figure(
                        "Return",
                        percent(figures.net_return),
                        up_or_down(figures.net_return),
                        "What the account made after all costs.",
                    ))
                    .push(figure(
                        "Before costs",
                        figures.gross_return.map_or("—".to_owned(), percent),
                        figures.gross_return.map_or(theme::TEXT, up_or_down),
                        "The same run with nothing to pay.",
                    ))
                    .push(figure(
                        "Equal shares",
                        percent(figures.benchmark_return),
                        up_or_down(figures.benchmark_return),
                        "An equal share of the capital held in each market from its first bar.",
                    ))
                    .push(figure(
                        "Sharpe",
                        figures.sharpe.map_or("—".to_owned(), |sharpe| format!("{sharpe:.2}")),
                        theme::TEXT,
                        "Return for the ups and downs taken, from daily returns over the span.",
                    ))
                    .push(figure(
                        "Market risk",
                        format!(
                            "{} · {:.1}%",
                            figures
                                .benchmark_sharpe
                                .map_or("—".to_owned(), |sharpe| format!("{sharpe:.2}")),
                            figures.benchmark_max_drawdown * 100.0
                        ),
                        theme::TEXT_MUTED,
                        "Equal shares' Sharpe ratio, then their deepest fall, worked out as the run's are: the risk the market took for its return, to set beside the run's.",
                    ))
                    .push(figure(
                        "Beta",
                        figures.beta.map_or("—".to_owned(), |beta| format!("{beta:.2}")),
                        theme::TEXT,
                        "How far the account moved with equal shares of its markets, day by day: 1 moves with them, 0 owes them nothing, below 0 moves against them.",
                    ))
                    .push(figure(
                        "Fell",
                        format!("{:.1}%", report.max_drawdown * 100.0),
                        theme::TEXT,
                        "The deepest fall from a peak.",
                    ))
                    .push(figure(
                        "Margin",
                        format!("{:.0}%", report.peak_margin_use * 100.0),
                        theme::TEXT,
                        "The most of the account that margin ever took.",
                    ))
                    .push(figure(
                        "Closest",
                        report.closest_to_liquidation.map_or("—".to_owned(), |room| format!("{room:.1}×")),
                        theme::TEXT,
                        "The least the account ever was, at the worst of a bar, as a multiple of what its positions needed to stay open. At 1× it's liquidated.",
                    ));
                let warnings: Vec<&str> = report
                    .notes
                    .iter()
                    .filter(|n| n.warning)
                    .map(|n| n.text.as_str())
                    .collect();
                if !warnings.is_empty() {
                    note = Some(text(warnings.join(" ")).size(SMALL).color(theme::DOWN));
                }
                row![
                    column![
                        container(with_legend(portfolio_equity(found)))
                            .height(iced::Length::FillPortion(3)),
                        container(with_legend(portfolio_exposure(report)))
                            .height(iced::Length::FillPortion(1)),
                    ]
                    .padding(Padding {
                        top: WALK_TOP,
                        ..Padding::ZERO
                    })
                    .width(Fill)
                    .height(Fill),
                    portfolio_table(
                        found,
                        panel.tests.portfolio_leg,
                        panel
                            .inspect
                            .as_ref()
                            .is_some_and(|i| i.what == InspectWhat::Portfolio),
                    ),
                ]
                .into()
            }
            TestRun::Running { done, total, .. } => {
                note = Some(muted(if *total == 0 {
                    "Reading the saved run…".to_owned()
                } else {
                    match info.settings.portfolio.legs.get(*done) {
                        Some(leg) => format!("Loading {}, {} of {total}…", leg.symbol, done + 1),
                        None => "Running them…".to_owned(),
                    }
                }));
                Space::new().width(Fill).height(Fill).into()
            }
            TestRun::Failed(message) => {
                note = Some(text(message.as_str()).size(SMALL).color(theme::DOWN));
                Space::new().width(Fill).height(Fill).into()
            }
            TestRun::Idle => {
                note = Some(muted(
                    "Add markets and weights, then press Run. Costs come from the stock settings."
                        .into(),
                ));
                Space::new().width(Fill).height(Fill).into()
            }
        },
        (status, TestKind::WalkForward) if !matches!(status, Status::NoStrategy) => {
            match &panel.tests.walk_forward {
                TestRun::Done(found) => {
                    heading = heading
                        .push(figure(
                            "Unseen",
                            percent(found.unseen_return),
                            up_or_down(found.unseen_return),
                            "What the unseen stretches returned, added up. This is the figure to trust: none of it was used to pick the values.",
                        ))
                        .push(figure(
                            "Market",
                            percent(found.buy_and_hold_return),
                            up_or_down(found.buy_and_hold_return),
                            "What the market did over the same stretches, added up.",
                        ))
                        .push(figure(
                            "Profitable",
                            format!("{} of {}", found.profitable_windows, found.windows.len()),
                            theme::TEXT,
                            "How many unseen stretches made money.",
                        ))
                        .push(figure(
                            "Worst fall",
                            format!("{:.1}%", found.deepest_fall * 100.0),
                            theme::TEXT,
                            "The deepest fall from a peak in any unseen stretch.",
                        ));
                    note = Some(muted(format!(
                        "Each window picks on {} bars, then is tested on the next {}. Each starts from the same capital. One after another, that's {} against {}.",
                        found.train_bars,
                        found.test_bars,
                        percent(found.compounded().0),
                        percent(found.compounded().1),
                    )));
                    row![
                        container(
                            canvas(WalkChart {
                                found,
                                market: "Equal shares, held"
                            })
                            .width(Fill)
                            .height(Fill)
                        )
                        .padding(Padding {
                            top: WALK_TOP,
                            ..Padding::ZERO
                        })
                        .width(Fill)
                        .height(Fill),
                        walk_forward_table(
                            found,
                            "Market",
                            InspectWhat::WalkWindow,
                            inspected_window(panel, false),
                        ),
                    ]
                    .into()
                }
                TestRun::Running { done, total, .. } => {
                    note = Some(muted(format!("Running backtest {done} of {total}…")));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Failed(message) => {
                    note = Some(
                        text(message.as_str())
                            .size(theme::TEXT_SIZE)
                            .color(theme::DOWN),
                    );
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Idle => {
                    if info.problem.is_none() {
                        note = Some(muted(
                            "Pick a variable and its range, then press Run.".to_owned(),
                        ));
                    }
                    Space::new().width(Fill).height(Fill).into()
                }
            }
        }
        (Status::Done(_), TestKind::RandomEntries) if panel.tests.entries_shifted => {
            match &panel.tests.shifted {
                TestRun::Done(found) => {
                    heading = heading
                        .push(figure("Backtest", percent(found.actual_return), up_or_down(found.actual_return), "What the backtest itself returned."))
                        .push(figure("Median", percent(found.median), up_or_down(found.median), "The return half the moved ones beat."))
                        .push(figure("As good", format!("{:.0}%", found.share_matching_return * 100.0), theme::TEXT, "The share of moved ones that returned at least as much. Near 50% means the timing added nothing."))
                        .push(figure("As safe", format!("{:.0}%", found.share_matching_fall * 100.0), theme::TEXT, "The share of moved ones that fell no further."));
                    note = Some(muted(if found.trades < 2 {
                        SHIFTED_ONE.to_owned()
                    } else {
                        format!(
                            "{} changes moved along the span {} times. 19 in 20 returned {} to {}.",
                            found.trades,
                            found.runs,
                            percent(found.low),
                            percent(found.high),
                        )
                    }));
                    shifted_charts(found)
                }
                TestRun::Running { done, total, .. } => {
                    note = Some(muted(format!("Running backtest {done} of {total}…")));
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Failed(message) => {
                    note = Some(
                        text(message.as_str())
                            .size(theme::TEXT_SIZE)
                            .color(theme::DOWN),
                    );
                    Space::new().width(Fill).height(Fill).into()
                }
                TestRun::Idle => {
                    note = Some(muted(
                        "Press Run to move this backtest's positions along the span.".to_owned(),
                    ));
                    Space::new().width(Fill).height(Fill).into()
                }
            }
        }
        (Status::Done(_), TestKind::RandomEntries) => match &panel.tests.random_entries {
            TestRun::Done(found) => {
                heading = heading
                    .push(figure(
                        "Backtest",
                        percent(found.actual_return),
                        up_or_down(found.actual_return),
                        "What the backtest itself returned.",
                    ))
                    .push(figure(
                        "Median",
                        percent(found.median),
                        up_or_down(found.median),
                        "The return half the random placings beat.",
                    ))
                    .push(figure(
                        "As good",
                        format!("{:.0}%", found.share_matching_return * 100.0),
                        theme::TEXT,
                        "The share of random placings that returned at least as much. Near 50% means the entries add nothing.",
                    ))
                    .push(figure(
                        "As safe",
                        format!("{:.0}%", found.share_matching_fall * 100.0),
                        theme::TEXT,
                        "The share of random placings that fell no further than the backtest did.",
                    ));
                note = Some(if found.trades < 2 {
                    text(ONE_TRADE).size(theme::TEXT_SIZE).color(theme::DOWN)
                } else {
                    muted(format!(
                        "{} trades placed at random {} times, holding each as long. 19 in 20 returned between {} and {}.",
                        found.trades,
                        found.runs,
                        percent(found.low),
                        percent(found.high),
                    ))
                });
                let (returns, falls) = (&found.returns, &found.falls);
                row![
                    canvas(Distribution {
                        title: "What random placings returned",
                        values: returns,
                        actual: found.actual_return,
                        band: Some((found.low, found.high)),
                        median: found.median,
                        pick: Some(|value| PanelMessage::Inspect(InspectWhat::RandomPlacing(
                            value
                        ))),
                    })
                    .width(FillPortion(3))
                    .height(Fill),
                    canvas(Distribution {
                        title: "How far each fell at worst",
                        values: falls,
                        actual: found.actual_fell,
                        band: None,
                        median: percentile(falls, 0.5),
                        pick: None,
                    })
                    .width(FillPortion(2))
                    .height(Fill),
                ]
                .into()
            }
            TestRun::Running { done, total, .. } => {
                note = Some(muted(format!("Running backtest {done} of {total}…")));
                Space::new().width(Fill).height(Fill).into()
            }
            TestRun::Failed(message) => {
                note = Some(
                    text(message.as_str())
                        .size(theme::TEXT_SIZE)
                        .color(theme::DOWN),
                );
                Space::new().width(Fill).height(Fill).into()
            }
            TestRun::Idle => {
                note = Some(muted(
                    "Press Run to place this backtest's trades at random.".to_owned(),
                ));
                Space::new().width(Fill).height(Fill).into()
            }
        },
        (status, TestKind::Optimize) if !matches!(status, Status::NoStrategy) => {
            match &panel.tests.optimize {
                OptimizeRun::Done(found) => {
                    let grid = &found.grid;
                    heading = heading.push(figure(
                        "Tried",
                        grid.cells().to_string(),
                        theme::TEXT,
                        "How many values were tried. The more there are, the more likely the best is only lucky.",
                    ));
                    if let Some(best) = found.top.first() {
                        heading = heading.push(figure(
                            "Best",
                            format!("{} {}", grid.label(best.cell), percent(best.score)),
                            up_or_down(best.score),
                            "The top value, ranked with its neighbours so a lone spike can't win.",
                        ));
                    }
                    if let Some(second) = &grid.second {
                        note = Some(muted(format!(
                            "{} across, {} up. Click a value to try it on the chart.",
                            grid.first.name, second.name
                        )));
                    } else {
                        note = Some(muted(format!(
                            "{} along the bottom. Click a value to try it on the chart.",
                            grid.first.name
                        )));
                    }
                    if found.top.is_empty() {
                        note = Some(
                            text("None of these values traded, so nothing could be ranked.")
                                .size(theme::TEXT_SIZE)
                                .color(theme::DOWN),
                        );
                    } else if found.on_edge {
                        note = Some(
                            text("The best is at the edge of the range, so better ones may lie beyond it. Try a wider range.")
                                .size(theme::TEXT_SIZE)
                                .color(theme::DOWN),
                        );
                    }
                    let current = grid.cell_of(info.params);
                    row![
                        canvas(Landscape { found, current })
                            .width(Fill)
                            .height(Fill),
                        results(found, current, panel.tests.original.is_some()),
                    ]
                    .into()
                }
                OptimizeRun::Running { done, total, .. } => {
                    note = Some(muted(format!("Running backtest {done} of {total}…")));
                    Space::new().width(Fill).height(Fill).into()
                }
                OptimizeRun::Failed(message) => {
                    note = Some(
                        text(message.as_str())
                            .size(theme::TEXT_SIZE)
                            .color(theme::DOWN),
                    );
                    Space::new().width(Fill).height(Fill).into()
                }
                OptimizeRun::Idle => {
                    // Otherwise the settings say why it can't.
                    if info.problem.is_none() {
                        note = Some(muted(
                            "Pick a variable and its range, then press Run.".to_owned(),
                        ));
                    }
                    Space::new().width(Fill).height(Fill).into()
                }
            }
        }
        (Status::Done(_) | Status::Running | Status::Waiting, TestKind::RandomBlocks) => {
            match &panel.tests.blocks {
                BlockRun::Done(summary) => {
                    heading = heading
                        .push(figure(
                            "Profitable",
                            format!("{:.0}%", summary.profitable * 100.0),
                            theme::TEXT,
                            "The share of blocks that made money. Near 50% suggests the strategy has no real edge in this market.",
                        ))
                        .push(figure(
                            "Median",
                            percent(summary.median),
                            up_or_down(summary.median),
                            "The return half the blocks beat.",
                        ))
                        .push(figure(
                            "Best",
                            percent(summary.best),
                            up_or_down(summary.best),
                            "The best block's return. Far above the median, the result leans on a few good stretches.",
                        ))
                        .push(figure(
                            "Worst",
                            percent(summary.worst),
                            up_or_down(summary.worst),
                            "The worst block's return: what a bad stretch of this market did.",
                        ));
                    note = Some(muted(format!(
                        "Each line is a real stretch of the market, backtested alone: {} of them. Click one to see it on the chart; scroll to zoom.",
                        summary.paths.len(),
                    )));
                    heading = heading.push(runs_view_chips(panel.tests.runs_table));
                    if panel.tests.runs_table {
                        note = None;
                        blocks_pane(
                            summary,
                            panel.tests.run,
                            panel.tests.runs_sort,
                            panel.tests.runs_scroll,
                            info.settings.capital,
                            panel.tests.trade_row,
                        )
                    } else {
                        row![
                            canvas(Fan {
                                paths: &summary.paths,
                                median: &summary.median_path,
                                actual: None,
                                stats: summary.blocks.iter().map(|b| b.stats).collect(),
                                marks: vec![
                                    (summary.best_line, theme::UP),
                                    (summary.worst_line, theme::DOWN),
                                ],
                                picked: panel.tests.line,
                                zoom: panel.tests.zoom.unwrap_or((0.0, 1.0)),
                                start: info.settings.capital,
                                unit: "Bar",
                                stride: summary.stride,
                                line_name: "This block",
                            })
                            .width(FillPortion(1))
                            .height(Fill),
                            match panel.tests.line.filter(|_| panel.tests.showing_trades) {
                                Some(line) => half(block_trades(
                                    summary,
                                    line,
                                    info.settings.capital,
                                    panel.tests.trade_row,
                                    Some(fan_switches(
                                        &[
                                            ("Best", summary.best_line),
                                            ("Worst", summary.worst_line)
                                        ],
                                        line,
                                        PanelMessage::ShowRuns,
                                        "Back to the list of every block. This line stays picked.",
                                    )),
                                )),
                                None => blocks_list(
                                    summary,
                                    panel.tests.line,
                                    panel.tests.zoom.is_some(),
                                ),
                            },
                        ]
                        .into()
                    }
                }
                BlockRun::Running { done, total, .. } => {
                    note = Some(muted(format!("Backtesting block {done} of {total}…")));
                    Space::new().width(Fill).height(Fill).into()
                }
                BlockRun::Failed(message) => {
                    note = Some(
                        text(message.as_str())
                            .size(theme::TEXT_SIZE)
                            .color(theme::DOWN),
                    );
                    Space::new().width(Fill).height(Fill).into()
                }
                BlockRun::Idle => {
                    // Otherwise the settings say why it can't.
                    if info.problem.is_none() {
                        note = Some(muted(if matches!(info.status, Status::Done(_)) {
                            format!(
                                "Press Run to backtest {} random stretches of the span.",
                                info.settings.blocks
                            )
                        } else {
                            "Waiting for the backtest…".to_owned()
                        }));
                    }
                    Space::new().width(Fill).height(Fill).into()
                }
            }
        }
        (status, _) => {
            note = Some(match status {
                Status::Failed(message) => text(*message).size(theme::TEXT_SIZE).color(theme::DOWN),
                Status::Running => muted("Backtesting…".into()),
                Status::NoStrategy => {
                    muted("Pick a strategy on the left to backtest this chart.".into())
                }
                _ => muted("Waiting for the chart's bars…".into()),
            });
            Space::new().width(Fill).height(Fill).into()
        }
    };
    // A run's table of trades leaves little room, and its heading has the run's figures.
    let table = panel.tests.showing_trades
        && !panel.tests.runs_table
        && match test {
            TestKind::MonteCarlo => {
                matches!(info.status, Status::Done(_))
                    && matches!(panel.tests.monte_carlo, Some(Ok(_)))
            }
            TestKind::RandomBlocks => matches!(panel.tests.blocks, BlockRun::Done(_)),
            _ => false,
        };
    let heading = if table {
        note = None;
        row![
            text(info.title.clone())
                .size(theme::LEGEND_SIZE)
                .color(theme::TEXT_BRIGHT),
            runs_view_chips(false),
        ]
        .spacing(14)
        .align_y(Alignment::Center)
    } else {
        heading
    };
    let mut legend = column![heading].spacing(5).padding(Padding {
        top: 8.0,
        right: 0.0,
        bottom: 0.0,
        left: 12.0,
    });
    if let Some(crumbs) = crumbs {
        legend = legend.push(crumbs);
    }
    if let Some(note) = note {
        legend = legend.push(note);
    }
    if let Some(fills) = info.fills.filter(|_| !table) {
        legend = legend.push(muted(fills));
    }
    let pane: Element<'_, PanelMessage> = stack![pane, legend].width(Fill).height(Fill).into();
    // A run's trades beside the test, when one's being looked into.
    let pane: Element<'_, PanelMessage> = match &panel.inspect {
        Some(inspection) => row![
            container(pane).width(FillPortion(3)).height(Fill),
            inspect_drawer(inspection, info.quote, panel.playback.at),
        ]
        .height(Fill)
        .into(),
        None => pane,
    };

    let label = |content: &'static str, what: &'static str| {
        tip(
            text(content)
                .size(theme::TEXT_SIZE)
                .color(theme::TEXT_MUTED)
                .width(40),
            what,
            None,
        )
    };
    let figures = |content: String| {
        text(content)
            .size(theme::TEXT_SIZE)
            .font(iced::Font::MONOSPACE)
            .color(theme::TEXT)
            .width(FIGURES_WIDTH)
    };
    let speed_row = row![
        transport,
        label(
            "Speed",
            "How fast playback runs: real time at the left, the whole span in two seconds at the right.",
        ),
        canvas(Speed {
            position: panel.playback.speed,
        })
        .width(Fill)
        .height(SPEED_HEIGHT),
        figures(speed_text(panel.playback.speed, info.bar_nanos)),
        Space::new().width(PRICE_AXIS_WIDTH),
    ]
    .spacing(8)
    .padding(Padding {
        left: 8.0,
        ..Padding::ZERO
    })
    .align_y(Alignment::Center)
    .height(SPEED_HEIGHT);

    // The span's slider, once the history is known.
    let (from, to) = panel
        .dragging
        .or(info.span)
        .or(info.traded)
        .unwrap_or((0, 0));
    let span_widget: Element<'_, PanelMessage> = match &panel.history {
        Some(Ok(history)) => canvas(Span {
            history,
            from,
            to,
            playhead: panel.playback.at,
        })
        .width(Fill)
        .height(SPAN_HEIGHT - 4.0)
        .into(),
        Some(Err(error)) => text(format!("The history couldn't be loaded: {error}"))
            .size(theme::TEXT_SIZE)
            .color(theme::DOWN)
            .width(Fill)
            .into(),
        None => muted("Finding the market's history…".into())
            .width(Fill)
            .into(),
    };
    let dates = if to > from {
        format!(
            "{} → {}",
            DateTime::from_nanos(from).date(),
            DateTime::from_nanos(to).date()
        )
    } else {
        String::new()
    };
    let latest = tip(
        button(text("Latest bars").size(theme::TEXT_SIZE).center())
            .width(LEFT_WIDTH)
            .padding([2, 0])
            .on_press_maybe(info.span.is_some().then_some(PanelMessage::Latest))
            .style(|_, status| plain(status)),
        "Test the chart's latest bars instead of the span chosen on the slider.",
        None,
    );
    // In the corner under the price axis, where a chart has its own buttons.
    let corner = container(tip(
        button(text("✕").size(13).center())
            .width(ICON_WIDTH)
            .height(20)
            .padding(0)
            .on_press(PanelMessage::Close)
            .style(|_, status| plain(status)),
        "Close the panel",
        Some("B"),
    ))
    .center_x(PRICE_AXIS_WIDTH);
    let span_row = row![
        latest,
        label(
            "Span",
            "The dates to test, out of the market's whole history: drag either handle, or the middle to move both. Bars before it only warm the strategy up.",
        ),
        span_widget,
        figures(dates),
        corner,
    ]
        .spacing(8)
        .padding(Padding {
            left: 8.0,
            ..Padding::ZERO
        })
        .align_y(Alignment::Center)
        .height(SPAN_HEIGHT);

    let main = column![pane, span_row, speed_row]
        .width(FillPortion(10 - SETTINGS_PORTION))
        .height(Fill);
    let divider = container(Space::new())
        .width(1)
        .height(Fill)
        .style(|_| container::background(theme::AXIS_LINE));
    let side = container(settings)
        .width(FillPortion(SETTINGS_PORTION))
        .height(Fill);
    container(column![
        row![side, divider, main].height(Fill),
        canvas(Edge).width(Fill).height(super::EDGE_HEIGHT)
    ])
    .width(Fill)
    .height(Fill)
    .style(|_| container::background(theme::BACKGROUND))
    .into()
}

/// A button showing a drawn icon, naming itself (and its shortcut) when hovered.
/// How many rows of limit orders to draw, which is plenty to read and keeps a redraw quick.
const LIMIT_ROWS: usize = 500;

/// Each limit order, under how those that filled did against those that missed.
fn limit_orders_table(report: &BacktestReport) -> Element<'_, PanelMessage> {
    let cell = |value: String, width: f32, color: Color| {
        text(value)
            .size(SMALL)
            .width(width)
            .color(color)
            .font(iced::Font::MONOSPACE)
    };
    let day = |ts: UnixNanos| {
        let at = DateTime::from_nanos(ts);
        format!("{} {}", at.date(), at.time())
    };
    let moved = |value: Option<f64>| value.map_or("—".to_owned(), |v| format!("{v:+.2}%"));
    let summary = LimitSummary::of(report);
    let mut rows = column![
        text(format!(
            "A bar on, those that filled moved {} their way, and those that missed {}. 24 bars on: {} and {}. {} of {} fills barely went past their price.",
            moved(summary.filled_move_1),
            moved(summary.missed_move_1),
            moved(summary.filled_move_24),
            moved(summary.missed_move_24),
            summary.barely_filled,
            summary.filled,
        ))
        .size(SMALL)
        .color(theme::TEXT),
        row![
            cell("Placed".into(), 150.0, theme::TEXT_MUTED),
            cell("Side".into(), 46.0, theme::TEXT_MUTED),
            cell("Price".into(), 92.0, theme::TEXT_MUTED),
            cell("Filled".into(), 64.0, theme::TEXT_MUTED),
            cell("Became".into(), 80.0, theme::TEXT_MUTED),
            cell("Reached".into(), 70.0, theme::TEXT_MUTED),
            cell("A bar on".into(), 72.0, theme::TEXT_MUTED),
            cell("24 on".into(), 72.0, theme::TEXT_MUTED),
        ],
    ]
    .spacing(3);
    for order in report.limit_log.iter().take(LIMIT_ROWS) {
        let (became, color) = match order.outcome {
            LimitOutcome::Filled => ("filled", theme::UP),
            LimitOutcome::Lapsed => ("lapsed", theme::TEXT_MUTED),
            LimitOutcome::Replaced => ("replaced", theme::TEXT_MUTED),
            LimitOutcome::Cancelled => ("cancelled", theme::TEXT_MUTED),
            LimitOutcome::Refused => ("refused", theme::DOWN),
            LimitOutcome::Open => ("open", theme::TEXT),
        };
        let side = if order.side == Side::Buy {
            "buy"
        } else {
            "sell"
        };
        rows = rows.push(row![
            cell(day(order.placed), 150.0, theme::TEXT),
            cell(side.to_owned(), 46.0, theme::TEXT),
            cell(format!("{:.2}", order.price), 92.0, theme::TEXT),
            cell(format!("{}", order.filled), 64.0, theme::TEXT),
            cell(became.to_owned(), 80.0, color),
            cell(format!("{:+.2}%", order.reached_percent), 70.0, theme::TEXT),
            cell(moved(order.move_1_percent), 72.0, theme::TEXT),
            cell(moved(order.move_24_percent), 72.0, theme::TEXT),
        ]);
    }
    if report.limit_log.len() > LIMIT_ROWS {
        rows = rows.push(
            text(format!(
                "And {} more. `get_limit_orders` in Lab has them all.",
                report.limit_log.len() - LIMIT_ROWS
            ))
            .size(SMALL)
            .color(theme::TEXT_MUTED),
        );
    }
    iced::widget::scrollable(rows.padding(Padding {
        top: WALK_TOP,
        right: 12.0,
        bottom: 8.0,
        left: 12.0,
    }))
    .width(Fill)
    .height(Fill)
    .into()
}

/// What's said when there's little timing to move.
const SHIFTED_ONE: &str =
    "It only changed once, so moving it says almost nothing about its timing.";

/// Each bar's decision, those where something was asked or sent first being what's worth reading.
fn decisions_table(report: &BacktestReport) -> Element<'_, PanelMessage> {
    let cell = |value: String, width: f32, color: Color| {
        text(value)
            .size(SMALL)
            .width(width)
            .color(color)
            .font(iced::Font::MONOSPACE)
    };
    let way = |position: Option<i8>| match position {
        Some(1) => "long",
        Some(-1) => "short",
        Some(_) => "flat",
        None => "—",
    };
    let mut rows = column![row![
        cell("Closed".into(), 150.0, theme::TEXT_MUTED),
        cell("Asked".into(), 60.0, theme::TEXT_MUTED),
        cell("Held".into(), 60.0, theme::TEXT_MUTED),
        cell("Rules".into(), 60.0, theme::TEXT_MUTED),
        cell("Order".into(), 100.0, theme::TEXT_MUTED),
        cell("Why".into(), 280.0, theme::TEXT_MUTED),
    ]]
    .spacing(3);
    let worth: Vec<_> = report
        .decisions
        .iter()
        .filter(|row| row.wanted.is_some() || row.order != 0.0)
        .collect();
    for decision in worth.iter().take(LIMIT_ROWS) {
        let at = DateTime::from_nanos(decision.ts);
        rows = rows.push(row![
            cell(format!("{} {}", at.date(), at.time()), 150.0, theme::TEXT),
            cell(way(decision.wanted).to_owned(), 60.0, theme::TEXT),
            cell(way(Some(decision.held)).to_owned(), 60.0, theme::TEXT),
            cell(way(decision.target).to_owned(), 60.0, theme::TEXT),
            cell(
                if decision.order == 0.0 {
                    "—".to_owned()
                } else {
                    format!("{:+}", decision.order)
                },
                100.0,
                theme::TEXT,
            ),
            cell(decision.why.to_owned(), 280.0, theme::TEXT_MUTED),
        ]);
    }
    if worth.len() > LIMIT_ROWS {
        rows = rows.push(cell(
            format!(
                "And {} more. `get_decisions` in Lab has them all.",
                worth.len() - LIMIT_ROWS
            ),
            500.0,
            theme::TEXT_MUTED,
        ));
    }
    scrollable(rows.padding(Padding {
        top: WALK_TOP,
        right: 12.0,
        bottom: 8.0,
        left: 12.0,
    }))
    .width(Fill)
    .height(Fill)
    .into()
}

/// A book moved along its span: what those returned and how far they fell.
/// Clicking where they returned looks into the one nearest, where they can be found again.
fn shifted_charts(found: &RandomEntries) -> Element<'_, PanelMessage> {
    let pick: Option<fn(f64) -> PanelMessage> = (!found.by_draw.is_empty())
        .then_some(|value| PanelMessage::Inspect(InspectWhat::ShiftedPlacing(value)));
    row![
        canvas(Distribution {
            title: "What the moved ones returned",
            values: &found.returns,
            actual: found.actual_return,
            band: Some((found.low, found.high)),
            median: found.median,
            pick,
        })
        .width(FillPortion(3))
        .height(Fill),
        canvas(Distribution {
            title: "How far each fell at worst",
            values: &found.falls,
            actual: found.actual_fell,
            band: None,
            median: percentile(&found.falls, 0.5),
            pick: None,
        })
        .width(FillPortion(2))
        .height(Fill),
    ]
    .into()
}

/// What random placings returned and how far they fell, a click on the returns opening the
/// placing nearest.
fn placing_charts(
    found: &RandomEntries,
    pick: fn(f64) -> PanelMessage,
) -> Element<'_, PanelMessage> {
    row![
        canvas(Distribution {
            title: "What the placings returned",
            values: &found.returns,
            actual: found.actual_return,
            band: Some((found.low, found.high)),
            median: found.median,
            pick: Some(pick),
        })
        .width(FillPortion(3))
        .height(Fill),
        canvas(Distribution {
            title: "How far each fell at worst",
            values: &found.falls,
            actual: found.actual_fell,
            band: None,
            median: percentile(&found.falls, 0.5),
            pick: None,
        })
        .width(FillPortion(2))
        .height(Fill),
    ]
    .into()
}

/// A sweep's values, best first.
/// Which walk-forward window's pick the drawer shows, if it shows one: a portfolio's, or one
/// market's.
fn inspected_window(panel: &Panel, portfolio: bool) -> Option<usize> {
    match (&panel.inspect.as_ref()?.what, portfolio) {
        (InspectWhat::PortfolioWalkWindow(at), true) | (InspectWhat::WalkWindow(at), false) => {
            Some(*at)
        }
        _ => None,
    }
}

fn portfolio_sweep_table(rows: &[SweepRow]) -> Element<'_, PanelMessage> {
    let cell = |value: String, width: f32, color: Color| {
        text(value)
            .size(SMALL)
            .width(width)
            .color(color)
            .font(iced::Font::MONOSPACE)
    };
    let mut lines = column![row![
        cell("Value".into(), 92.0, theme::TEXT_MUTED),
        cell("Return".into(), 76.0, theme::TEXT_MUTED),
        cell("Sharpe".into(), 66.0, theme::TEXT_MUTED),
        tip(
            cell("Deflated".into(), 66.0, theme::TEXT_MUTED),
            "For the best: the chance its true Sharpe ratio is above nothing, allowing for how many setups were tried, how much their Sharpe ratios differed, and how far its daily returns were from normal. Under 95% is weak evidence.",
            None,
        ),
        cell("Fell".into(), 66.0, theme::TEXT_MUTED),
        cell("Turnover".into(), 76.0, theme::TEXT_MUTED),
        cell("Margin".into(), 66.0, theme::TEXT_MUTED),
        cell("".into(), 90.0, theme::TEXT_MUTED),
    ]]
    .spacing(3);
    for found in rows {
        let color = if found.net_return >= 0.0 {
            theme::UP
        } else {
            theme::DOWN
        };
        let line = row![
            cell(found.label.clone(), 92.0, theme::TEXT),
            cell(percent(found.net_return), 76.0, color),
            cell(
                found
                    .sharpe
                    .map_or("—".to_owned(), |sharpe| format!("{sharpe:.2}")),
                66.0,
                theme::TEXT
            ),
            cell(
                found
                    .deflated_sharpe
                    .map_or(String::new(), |chance| format!("{:.0}%", chance * 100.0)),
                66.0,
                theme::TEXT_MUTED
            ),
            cell(format!("{:.1}%", found.fell * 100.0), 66.0, theme::TEXT),
            cell(format!("{:.1}×", found.turnover), 76.0, theme::TEXT),
            cell(
                format!("{:.0}%", found.peak_margin_use * 100.0),
                66.0,
                theme::TEXT
            ),
            cell(
                if found.liquidated {
                    "liquidated".into()
                } else {
                    String::new()
                },
                90.0,
                theme::DOWN
            ),
        ];
        lines = lines.push(tip(
            button(line)
                .padding(0)
                .on_press(PanelMessage::Inspect(InspectWhat::PortfolioSweep(
                    found.label.clone(),
                )))
                .style(|_, status| result_style(false, status)),
            "Click to see this setup's trades, and follow one on the chart.",
            None,
        ));
    }
    iced::widget::scrollable(lines.padding(Padding {
        top: WALK_TOP,
        right: 12.0,
        bottom: 8.0,
        left: 12.0,
    }))
    .width(Fill)
    .height(Fill)
    .into()
}

/// Where a portfolio's draws stayed, with the run's own.
fn portfolio_band(found: &PortfolioMonteCarlo, capital: f64) -> Lines<'_> {
    Lines {
        lines: vec![
            (
                "Low".to_owned(),
                found.low_line.as_slice(),
                theme::TEXT_MUTED,
            ),
            (
                "High".to_owned(),
                found.high_line.as_slice(),
                theme::TEXT_MUTED,
            ),
            (
                "Median".to_owned(),
                found.median_line.as_slice(),
                leg_color(1),
            ),
            (
                "This run".to_owned(),
                found.actual_line.as_slice(),
                theme::TEXT_BRIGHT,
            ),
        ],
        times: &[],
        start: capital,
        format: |value| format!("{value:.0}"),
    }
}

/// A chart of lines under their names: each with a swatch of its color, spaced apart and wrapping
/// onto another row when there are too many for one.
fn with_legend(lines: Lines<'_>) -> Element<'_, PanelMessage> {
    let legend = row(lines.lines.iter().map(|(name, _, color)| {
        let color = *color;
        row![
            container(Space::new().width(8).height(8)).style(move |_| container::Style {
                background: Some(color.into()),
                ..container::Style::default()
            }),
            text(name.clone()).size(theme::TEXT_SIZE).color(color),
        ]
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
    }))
    .spacing(12)
    .padding(Padding {
        left: 12.0,
        ..Padding::ZERO
    })
    .wrap()
    .vertical_spacing(2);
    column![legend, canvas(lines).width(Fill).height(Fill)]
        .spacing(2)
        .into()
}

/// The account, what equal shares would have done, and what each leg has made.
fn portfolio_equity(found: &PortfolioRun) -> Lines<'_> {
    let report = &found.report;
    let mut lines = vec![
        (
            "Account".to_owned(),
            report.equity.as_slice(),
            theme::TEXT_BRIGHT,
        ),
        (
            "Equal shares".to_owned(),
            found.figures.benchmark.as_slice(),
            theme::TEXT_MUTED,
        ),
    ];
    lines.extend(report.legs.iter().enumerate().map(|(at, leg)| {
        (
            leg.symbol.clone(),
            leg.account_line.as_slice(),
            leg_color(at),
        )
    }));
    Lines {
        lines,
        times: &report.times,
        start: report.starting_balance,
        format: |value| format!("{value:.0}"),
    }
}

/// What's held long plus short, and long minus short, as shares of the account.
fn portfolio_exposure(report: &PortfolioReport) -> Lines<'_> {
    Lines {
        lines: vec![
            (
                "Gross".to_owned(),
                report.gross_exposure.as_slice(),
                leg_color(1),
            ),
            (
                "Net".to_owned(),
                report.net_exposure.as_slice(),
                leg_color(3),
            ),
        ],
        times: &report.times,
        start: 0.0,
        format: |value| format!("{:.0}%", value * 100.0),
    }
}

/// Each leg's part, and how their returns moved together.
fn portfolio_table(
    found: &PortfolioRun,
    picked: usize,
    inspecting: bool,
) -> Element<'_, PanelMessage> {
    let report = &found.report;
    let cell = |value: String, width: f32, color: Color| {
        text(value)
            .size(SMALL)
            .width(width)
            .color(color)
            .font(iced::Font::MONOSPACE)
    };
    let mut rows = column![row![
        cell("Market".into(), 112.0, theme::TEXT_MUTED),
        cell("Made".into(), 76.0, theme::TEXT_MUTED),
        cell("Fees".into(), 56.0, theme::TEXT_MUTED),
        cell("Funding".into(), 56.0, theme::TEXT_MUTED),
        cell("Borrow".into(), 50.0, theme::TEXT_MUTED),
        cell("Fills".into(), 40.0, theme::TEXT_MUTED),
    ]]
    .spacing(3);
    for (at, leg) in report.legs.iter().enumerate() {
        let made = leg.contribution.last().copied().unwrap_or(0.0);
        rows = rows.push(row![
            cell(leg.symbol.clone(), 112.0, leg_color(at)),
            cell(
                format!("{made:+.0}"),
                76.0,
                if made >= 0.0 { theme::UP } else { theme::DOWN }
            ),
            cell(format!("{:.0}", leg.fees), 56.0, theme::TEXT),
            cell(format!("{:.0}", leg.funding_paid), 56.0, theme::TEXT),
            cell(format!("{:.0}", leg.borrow_paid), 50.0, theme::TEXT),
            cell(leg.fills.len().to_string(), 40.0, theme::TEXT),
        ]);
    }
    rows = rows.push(Space::new().height(8));
    rows = rows.push(row![
        cell("Worked".into(), 112.0, theme::TEXT_MUTED),
        cell("Orders".into(), 56.0, theme::TEXT_MUTED),
        cell("Bars".into(), 56.0, theme::TEXT_MUTED),
        cell("Longest".into(), 62.0, theme::TEXT_MUTED),
        cell("To go".into(), 62.0, theme::TEXT_MUTED),
        cell("Gaps".into(), 40.0, theme::TEXT_MUTED),
    ]);
    for (at, leg) in report.legs.iter().enumerate() {
        rows = rows.push(row![
            cell(leg.symbol.clone(), 112.0, leg_color(at)),
            cell(leg.orders_worked.to_string(), 56.0, theme::TEXT),
            cell(
                format!("{:.1}", leg.bars_to_fill_average),
                56.0,
                theme::TEXT
            ),
            cell(leg.bars_to_fill_longest.to_string(), 62.0, theme::TEXT),
            cell(format!("{}", leg.unfilled_at_end), 62.0, theme::TEXT),
            cell(leg.missing_bars.to_string(), 40.0, theme::TEXT),
        ]);
    }
    rows = rows.push(Space::new().height(8));
    let mut heading = row![cell("Moved with".into(), 112.0, theme::TEXT_MUTED)];
    for (at, _) in report.legs.iter().enumerate() {
        heading = heading.push(cell(format!("{}", at + 1), 50.0, leg_color(at)));
    }
    rows = rows.push(heading);
    for (at, line) in found.figures.correlation.iter().enumerate() {
        let mut shown = row![cell(
            format!("{} {}", at + 1, report.legs[at].symbol),
            112.0,
            leg_color(at)
        )];
        for value in line {
            shown = shown.push(cell(
                value.map_or("—".to_owned(), |v| format!("{v:+.2}")),
                50.0,
                theme::TEXT,
            ));
        }
        rows = rows.push(shown);
    }
    rows = rows.push(Space::new().height(8));
    let costs = &found.figures.costs;
    rows = rows.push(cell(
        format!(
            "Costs {:.0}: fees {:.0}, slippage {:.0}, spread {:.0}, range {:.0}, impact {:.0}, funding {:.0}, borrowing {:.0}, interest {:.0}. Turned over {:.1}×. {} orders.",
            found.figures.costs_paid,
            costs.fees(),
            costs.slippage + costs.rounding,
            costs.spread,
            costs.range_slippage,
            costs.impact,
            costs.funding,
            costs.borrow,
            costs.interest,
            report.turnover,
            report.total_orders
        ),
        398.0,
        theme::TEXT_MUTED,
    ));
    let figures = &found.figures;
    rows = rows.push(cell(
        format!(
            "Equal shares after funding {}. {}",
            percent(figures.benchmark_after_funding),
            figures
                .weights_held_return
                .map_or(String::new(), |held| format!(
                    "These weights held {}.",
                    percent(held)
                )),
        ),
        398.0,
        theme::TEXT_MUTED,
    ));
    let mut years = column![row![
        cell("By year".into(), 112.0, theme::TEXT_MUTED),
        cell("Of capital".into(), 76.0, theme::TEXT_MUTED),
        cell("On the year".into(), 90.0, theme::TEXT_MUTED),
        cell("Equal shares".into(), 96.0, theme::TEXT_MUTED),
    ]]
    .spacing(3);
    let sign = |value: f64| if value >= 0.0 { theme::UP } else { theme::DOWN };
    for (year, made) in &found.figures.by_year {
        let compounded = found
            .figures
            .by_year_compounded
            .get(year)
            .copied()
            .flatten();
        let market = found
            .figures
            .benchmark_by_year_compounded
            .get(year)
            .copied()
            .flatten();
        years = years.push(row![
            cell(year.to_string(), 112.0, theme::TEXT),
            cell(percent(*made), 76.0, sign(*made)),
            cell(
                compounded.map_or_else(|| "–".into(), percent),
                90.0,
                compounded.map_or(theme::TEXT_MUTED, sign)
            ),
            cell(
                market.map_or_else(|| "–".into(), percent),
                96.0,
                theme::TEXT_MUTED
            ),
        ]);
    }
    rows = rows.push(Space::new().height(8)).push(years);
    rows = rows.push(
        container(
            canvas(BarsByTime::hours(figures.by_hour.clone()))
                .width(Fill)
                .height(Fill),
        )
        .width(398.0)
        .height(64.0),
    );
    rows = rows.push(
        container(
            canvas(BarsByTime::weekdays(figures.by_weekday.clone()))
                .width(Fill)
                .height(Fill),
        )
        .width(398.0)
        .height(64.0),
    );
    for note in report.notes.iter().filter(|note| !note.warning) {
        rows = rows.push(
            text(note.text.as_str())
                .size(SMALL)
                .color(theme::TEXT_MUTED)
                .width(398.0),
        );
    }
    // Every trade across the markets, to follow on a chart of its own market; then one market's
    // fills at a time, as there can be thousands.
    let mut tabs = row![tip(
        button(text("Trades").size(SMALL))
            .padding([2, 6])
            .on_press(if inspecting {
                PanelMessage::CloseInspection
            } else {
                PanelMessage::Inspect(InspectWhat::Portfolio)
            })
            .style(move |_, status| chip(inspecting, status)),
        "Every trade across the markets, flat to flat, beside the test: click one to follow it on a chart of its market, then Play.",
        None,
    )]
    .spacing(4);
    for (at, leg) in report.legs.iter().enumerate() {
        tabs = tabs.push(
            button(text(leg.symbol.as_str()).size(SMALL))
                .padding([2, 6])
                .on_press(PanelMessage::PortfolioLeg(at))
                .style(move |_, status| chip(at == picked, status)),
        );
    }
    rows = rows.push(Space::new().height(8)).push(tabs);
    if let Some(leg) = report.legs.get(picked) {
        rows = rows.push(row![
            cell("When".into(), 130.0, theme::TEXT_MUTED),
            cell("Side".into(), 40.0, theme::TEXT_MUTED),
            cell("Price".into(), 84.0, theme::TEXT_MUTED),
            cell("Size".into(), 84.0, theme::TEXT_MUTED),
            cell("Fee".into(), 56.0, theme::TEXT_MUTED),
        ]);
        for fill in leg.fills.iter().take(LIMIT_ROWS) {
            let at = DateTime::from_nanos(fill.ts);
            let side = if fill.side == Side::Buy {
                "buy"
            } else {
                "sell"
            };
            rows = rows.push(row![
                cell(format!("{} {}", at.date(), at.time()), 130.0, theme::TEXT),
                cell(side.to_owned(), 40.0, theme::TEXT),
                cell(format!("{:.2}", fill.price), 84.0, theme::TEXT),
                cell(format!("{}", fill.qty), 84.0, theme::TEXT),
                cell(format!("{:.2}", fill.fee), 56.0, theme::TEXT),
            ]);
        }
        if leg.fills.len() > LIMIT_ROWS {
            rows = rows.push(cell(
                format!(
                    "And {} more. `get_portfolio_fills` in Lab has them all.",
                    leg.fills.len() - LIMIT_ROWS
                ),
                398.0,
                theme::TEXT_MUTED,
            ));
        }
    }
    iced::widget::scrollable(rows.padding(Padding {
        top: WALK_TOP,
        right: 12.0,
        bottom: 8.0,
        left: 12.0,
    }))
    .width(WALK_TABLE_WIDTH)
    .height(Fill)
    .into()
}

/// Each window of a walk-forward: what it picked, and how that did on what it hadn't seen.
/// `inspect` is what clicking a window's row opens: its pick's trades on its unseen stretch.
fn walk_forward_table<'a>(
    found: &'a WalkForward,
    market: &str,
    inspect: fn(usize) -> InspectWhat,
    picked: Option<usize>,
) -> Element<'a, PanelMessage> {
    let day = |ts: UnixNanos| DateTime::from_nanos(ts).date();
    let cell = |value: String, width: f32, color: Color| {
        text(value)
            .size(SMALL)
            .width(width)
            .color(color)
            .font(iced::Font::MONOSPACE)
    };
    let mut rows = column![row![
        cell("From".into(), 84.0, theme::TEXT_MUTED),
        cell("Picked".into(), 92.0, theme::TEXT_MUTED),
        cell("Return".into(), 66.0, theme::TEXT_MUTED),
        cell(market.to_owned(), 66.0, theme::TEXT_MUTED),
        cell("Fell".into(), 50.0, theme::TEXT_MUTED),
        cell("Trades".into(), 46.0, theme::TEXT_MUTED),
        tip(
            cell("Deflated".into(), 60.0, theme::TEXT_MUTED),
            "Where it was picked: the chance its Sharpe ratio there was more than luck, for having been the best of all the values tried here. Anything you tried before makes it lower still.",
            None,
        ),
    ]]
    .spacing(3);
    for (at, window) in found.windows.iter().enumerate() {
        let stretch = day(window.test_from);
        rows = rows.push(match &window.picked {
            Some(picked_here) => {
                let picked_now = picked == Some(at);
                let picked = picked_here;
                let color = if picked.unseen_return >= 0.0 {
                    theme::UP
                } else {
                    theme::DOWN
                };
                let line = row![
                    cell(stretch, 84.0, theme::TEXT),
                    cell(picked.label.clone(), 92.0, theme::TEXT),
                    cell(percent(picked.unseen_return), 66.0, color),
                    cell(percent(picked.buy_and_hold_return), 66.0, theme::TEXT),
                    cell(format!("{:.1}%", picked.fell * 100.0), 50.0, theme::TEXT),
                    cell(picked.trades.to_string(), 46.0, theme::TEXT),
                    cell(
                        picked.deflated_sharpe.map_or_else(
                            || "–".to_owned(),
                            |chance| format!("{:.0}%", chance * 100.0)
                        ),
                        60.0,
                        theme::TEXT_MUTED
                    ),
                ];
                let line: Element<'a, PanelMessage> = tip(
                    button(line)
                        .padding(0)
                        .on_press(PanelMessage::Inspect(inspect(at)))
                        .style(move |_, status| result_style(picked_now, status)),
                    "Click to see this pick's trades on its unseen stretch, and follow one on the chart.",
                    None,
                );
                line
            }
            None => row![
                cell(stretch, 84.0, theme::TEXT),
                cell("nothing traded".into(), 200.0, theme::TEXT_MUTED),
            ]
            .into(),
        });
    }
    if let Some(sharpe) = found.unseen_sharpe {
        rows = rows.push(cell(
            format!("Unseen Sharpe {sharpe:.2}, needing no deflating as nothing was picked on it."),
            420.0,
            theme::TEXT_MUTED,
        ));
    }
    // Below the legend, which sits over the top of the pane.
    iced::widget::scrollable(rows.padding(Padding {
        top: WALK_TOP,
        right: 12.0,
        bottom: 8.0,
        left: 12.0,
    }))
    .width(WALK_TABLE_WIDTH)
    .height(Fill)
    .into()
}

fn icon_button<'a>(
    glyph: Glyph,
    what: &'a str,
    key: Option<&'a str>,
    on_press: Option<PanelMessage>,
    lit: bool,
) -> Element<'a, PanelMessage> {
    let enabled = on_press.is_some();
    tip(
        button(
            canvas(Icon {
                glyph,
                enabled,
                lit,
            })
            .width(Fill)
            .height(Fill),
        )
        .width(ICON_WIDTH)
        .height(22)
        .padding(0)
        .on_press_maybe(on_press)
        .style(|_, _| button::Style::default()),
        what,
        key,
    )
}

/// `control`, naming itself and its Control-key shortcut when hovered, like a chart's corner
/// buttons.
fn tip<'a>(
    control: impl Into<Element<'a, PanelMessage>>,
    what: impl text::IntoFragment<'a>,
    key: Option<&'a str>,
) -> Element<'a, PanelMessage> {
    tip_at(control, what, key, tooltip::Position::Bottom)
}

/// [`tip`], for a control that opens a list below itself, which the tip mustn't cover.
fn tip_at<'a>(
    control: impl Into<Element<'a, PanelMessage>>,
    what: impl text::IntoFragment<'a>,
    key: Option<&'a str>,
    position: tooltip::Position,
) -> Element<'a, PanelMessage> {
    let modifier = if cfg!(target_os = "macos") {
        "⌃"
    } else {
        "Ctrl+"
    };
    let mut label = row![text(what).size(12).color(theme::TEXT_BRIGHT)].spacing(8);
    if let Some(key) = key {
        label = label.push(
            text(format!("{modifier}{key}"))
                .size(12)
                .color(theme::TEXT_MUTED)
                .font(iced::Font::MONOSPACE),
        );
    }
    let bubble = container(label)
        .max_width(320)
        .padding([3, 7])
        .style(|_| container::Style {
            background: Some(Background::Color(theme::LABEL_BACKGROUND)),
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    tooltip(control, bubble, position).gap(4).into()
}

/// No fill or border: just the text, brighter under the mouse, as on a chart's corner buttons.
fn plain(status: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: match status {
            button::Status::Hovered | button::Status::Pressed => theme::TEXT_BRIGHT,
            button::Status::Disabled => theme::LABEL_BACKGROUND,
            button::Status::Active => theme::TEXT_MUTED,
        },
        border: Border::default(),
        ..button::Style::default()
    }
}

/// `items` laid out in rows of `columns`.
fn grid(items: Vec<Element<'_, PanelMessage>>, columns: usize) -> Element<'_, PanelMessage> {
    let mut items = items.into_iter().peekable();
    let mut rows = column![].spacing(7);
    while items.peek().is_some() {
        let mut cells = row![].spacing(20);
        for _ in 0..columns {
            cells = cells.push(match items.next() {
                Some(item) => container(item).width(Fill),
                None => container(Space::new()).width(Fill),
            });
        }
        rows = rows.push(cells);
    }
    rows.into()
}

/// A plugin on the backtest: an outlined pill, like a chart's tag for something switched off,
/// as these don't do anything yet.
fn tag_style(status: button::Status) -> button::Style {
    let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: hot.then_some(Background::Color(HOVER)),
        text_color: theme::TEXT_BRIGHT,
        border: Border {
            color: PANEL_BORDER,
            width: 1.0,
            radius: 9.0.into(),
        },
        ..button::Style::default()
    }
}

fn switch_style(theme: &iced::Theme, status: toggler::Status) -> toggler::Style {
    let mut style = toggler::default(theme, status);
    if matches!(
        status,
        toggler::Status::Active { is_toggled: true }
            | toggler::Status::Hovered { is_toggled: true }
    ) {
        style.background = Background::Color(theme::ACCENT);
    }
    style
}

fn list_style(status: pick_list::Status) -> pick_list::Style {
    let open = !matches!(status, pick_list::Status::Active);
    pick_list::Style {
        text_color: theme::TEXT_BRIGHT,
        placeholder_color: theme::TEXT_MUTED,
        handle_color: theme::TEXT_MUTED,
        background: Background::Color(theme::BACKGROUND),
        border: Border {
            color: if open { theme::ACCENT } else { PANEL_BORDER },
            width: 1.0,
            radius: 3.0.into(),
        },
    }
}

fn list_menu_style() -> overlay::menu::Style {
    overlay::menu::Style {
        background: Background::Color(PANEL),
        border: Border {
            color: PANEL_BORDER,
            width: 1.0,
            radius: 4.0.into(),
        },
        text_color: theme::TEXT_BRIGHT,
        selected_text_color: Color::WHITE,
        selected_background: Background::Color(theme::ACCENT),
        shadow: iced::Shadow::default(),
    }
}

/// The kinds of backtest plugin to come, and what each would do.
pub const PLUGIN_SLOTS: [(&str, &str); 7] = [
    (
        "Slippage model",
        "Click to take it off. Slippage from the order and the market, e.g. growing fast once an order is more than 5% of the bar's volume.",
    ),
    (
        "Fill model",
        "Click to take it off. Order-book or queue simulation: partial fills, or limit orders that fill only once the price trades through them.",
    ),
    (
        "Spread and latency",
        "Click to take it off. Spreads and delays that change, e.g. widening by 10 pips and lagging 200 ms when an economic calendar says CPI is out.",
    ),
    (
        "Position sizing",
        "Click to take it off. How big each position is: Kelly, volatility or ATR targeting, risk per trade. Without one, the strategy's Size % applies.",
    ),
    (
        "Holding costs",
        "Click to take it off. Overnight swaps or funding from your broker's history, charged on positions held.",
    ),
    (
        "Commission schedule",
        "Click to take it off. Tiered or broker-specific fees, e.g. cheaper as monthly volume grows.",
    ),
    (
        "Session calendar",
        "Click to take it off. Exchange holidays and half days, on top of the trading session.",
    ),
];

fn chip(selected: bool, status: button::Status) -> button::Style {
    let background = if selected {
        theme::ACCENT
    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        HOVER
    } else {
        Color::TRANSPARENT
    };
    let text_color = match status {
        button::Status::Disabled => theme::TEXT_MUTED,
        _ if selected => Color::WHITE,
        _ => theme::TEXT_BRIGHT,
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color,
        border: Border {
            color: if selected {
                theme::ACCENT
            } else {
                PANEL_BORDER
            },
            width: 1.0,
            radius: 4.0.into(),
        },
        ..button::Style::default()
    }
}

fn input_style(status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: Background::Color(theme::BACKGROUND),
        border: Border {
            color: if focused { theme::ACCENT } else { PANEL_BORDER },
            width: 1.0,
            radius: 3.0.into(),
        },
        icon: theme::TEXT_MUTED,
        placeholder: theme::TEXT_MUTED,
        value: theme::TEXT_BRIGHT,
        selection: Color {
            a: 0.4,
            ..theme::ACCENT
        },
    }
}
