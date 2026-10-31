//! The panel and its settings sheet, laid out.

use iced::{
    Alignment, Background, Border, Color, Element, Fill, FillPortion, Padding, overlay,
    widget::{
        Space, button, canvas, checkbox, column, combo_box, container, pick_list, row, scrollable,
        sensor, stack, text, text_input, toggler, tooltip,
    },
};
use persephone_chart::{PRICE_AXIS_WIDTH, theme};
use persephone_core::{BarSpec, Candle, UnixNanos, time::DateTime};
use persephone_engine::BacktestReport;

use super::{
    BlockLength, BlockRun, Commission, Edit, Field, NO_VARIABLE, OneWay, OptimizeRun, Panel,
    PanelMessage, Precision, RunColumn, ScoreBy, Settings, StrategyChoice, TestKind,
    calendar::{Calendar, Level, Total, days_in_month, weeks},
    optimize::{OptimizeResult, Outcome},
    progress,
    robustness::Problem,
    robustness::{
        BlockStats, BlockSummary, LineStats, MonteCarlo, RoundTrip, RunRow, RunSummary,
        round_trips, sort_rows, summarize_run,
    },
    speed_text,
    widgets::heat,
    widgets::{
        Edge, Equity, Fan, Glyph, HeatScale, Icon, Landscape, RunsTable, Span, Speed, percent,
    },
};

mod calendar;
mod results;
mod settings;

use self::{calendar::*, results::*, settings::*};

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
}

pub fn panel(info: PanelInfo<'_>) -> Element<'_, PanelMessage> {
    let panel = info.panel;
    let settings = settings_section(&info);
    let test = info.settings.test;
    // Only the backtest itself plays back.
    let ready = test == TestKind::Backtest && matches!(info.status, Status::Done(_));
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
        let what = if started {
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
    let title = text(info.title.clone())
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
                    "Trades",
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
            let view = panel.calendar;
            heading = heading.push(
                row![
                    tip(
                        button(text("Equity").size(HINT).center())
                            .padding([2, 7])
                            .on_press(PanelMessage::ShowCalendar(false))
                            .style(move |_, status| chip(!view.shown, status)),
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
            if view.shown {
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
    let pane = stack![pane, legend].width(Fill).height(Fill);

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
