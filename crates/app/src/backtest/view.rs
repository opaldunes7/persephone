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
    PanelMessage, Precision, ScoreBy, Settings, StrategyChoice, TestKind,
    calendar::{Calendar, Level, Total, days_in_month, weeks},
    optimize::{OptimizeResult, Outcome},
    progress,
    robustness::Problem,
    robustness::{BlockSummary, LineStats, MonteCarlo, RoundTrip, round_trips},
    speed_text,
    widgets::heat,
    widgets::{Edge, Equity, Fan, Glyph, HeatScale, Icon, Landscape, Span, Speed},
};

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
    let percent = |value: f64| format!("{:+.2}%", value * 100.0);
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
                    .width(Fill)
                    .height(Fill),
                    monte_carlo_list(result, panel.tests.line, panel.tests.zoom.is_some()),
                ]
                .into()
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
                        .width(Fill)
                        .height(Fill),
                        blocks_list(summary, panel.tests.line, panel.tests.zoom.is_some()),
                    ]
                    .into()
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
                        note = Some(muted(format!(
                            "Press Run to backtest {} random stretches of the span.",
                            info.settings.blocks
                        )));
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
    if let Some(fills) = info.fills {
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

/// The panel's left side: the strategy, every stock setting and the plugin slots, packed into
/// as many columns as fit. What each one does is in the tooltips of its name and controls.
fn settings_section<'a>(info: &PanelInfo<'a>) -> Element<'a, PanelMessage> {
    let (panel, settings) = (info.panel, info.settings);
    let stock = settings.stock;
    let input = |field: Field, width: f32| {
        let typed = panel
            .input(field)
            .map_or_else(|| settings.text(field), str::to_owned);
        text_input("", &typed)
            .on_input(move |t| PanelMessage::Input(field, t))
            .on_submit(PanelMessage::Submit(field))
            .padding([3, 6])
            .size(SMALL)
            .width(width)
            .style(|_, status| input_style(status))
    };
    let unit = |content: &'a str| text(content).size(HINT).color(theme::TEXT_MUTED);
    let name = |content: &'a str, what: &'a str| -> Element<'a, PanelMessage> {
        tip(
            text(content).size(SMALL).color(theme::TEXT_BRIGHT),
            what,
            None,
        )
    };
    let switch = |on: bool, edit: fn(bool) -> Edit, what: &'a str| -> Element<'a, PanelMessage> {
        tip(
            toggler(on)
                .size(15)
                .on_toggle_maybe(stock.then_some(move |on| PanelMessage::Edit(edit(on))))
                .style(switch_style),
            what,
            None,
        )
    };
    let no_switch = || -> Element<'a, PanelMessage> { Space::new().width(0).into() };
    // One setting: its switch, its name, and its controls at the right.
    let line = |toggle: Element<'a, PanelMessage>,
                name: Element<'a, PanelMessage>,
                controls: Element<'a, PanelMessage>|
     -> Element<'a, PanelMessage> {
        row![toggle, name, container(controls).align_right(Fill),]
            .spacing(7)
            .align_y(Alignment::Center)
            .into()
    };
    let chip_button = |label: &'a str, selected: bool, message: PanelMessage, what: &'a str| {
        tip(
            button(text(label).size(SMALL).center())
                .padding([3, 6])
                .on_press(message)
                .style(move |_, status| chip(selected, status)),
            what,
            None,
        )
    };
    let choice = |label: &'a str, selected: bool, edit: Edit, what: &'a str| {
        chip_button(label, selected, PanelMessage::Edit(edit), what)
    };

    let strategy = pick_list(
        info.strategies.clone(),
        info.strategy.clone(),
        PanelMessage::Strategy,
    )
    .placeholder("Pick a strategy…")
    .text_size(SMALL)
    .padding([4, 8])
    .width(Fill)
    .style(|_, status| list_style(status))
    .menu_style(|_| list_menu_style());
    let top = tip_at(
        strategy,
        "The strategy this chart trades and tests. Its settings are in the chart's right-click menu.",
        None,
        tooltip::Position::Right,
    );
    let open = settings.stock_open;
    let mut stock_title = row![tip(
        button(
            row![
                text(if open { "▾" } else { "▸" }).size(SMALL + 4.0),
                text("Stock settings").size(SMALL),
            ]
            .spacing(5)
            .align_y(Alignment::Center),
        )
        .padding(0)
        .on_press(PanelMessage::StockOpen(!open))
        .style(|_, status| plain(status)),
        if open {
            "Fold the stock settings away, for more room. They still apply."
        } else {
            "Show the stock settings."
        },
        None,
    )]
    .spacing(8)
    .align_y(Alignment::Center);
    if !open {
        stock_title = stock_title.push(
            text(format!(
                "{} of 7 on, capital {}",
                settings.stock_sections_on(),
                settings.text(Field::Capital)
            ))
            .size(HINT)
            .color(theme::TEXT_MUTED),
        );
    }
    let stock_title = stock_title.push(
        container(tip(
            toggler(stock)
                .size(15)
                .on_toggle(|on| PanelMessage::Edit(Edit::Stock(on)))
                .style(switch_style),
            "Off turns all the stock settings off at once, for an ideal run to compare against.",
            None,
        ))
        .align_right(Fill),
    );

    // Which test, and its own settings.
    let tests = row![
        name(
            "Test",
            "How to test the strategy. One backtest can't tell skill from luck; the other two show how far its result could have gone either way.",
        ),
        container(
            row![
                chip_button(
                    "Backtest",
                    settings.test == TestKind::Backtest,
                    PanelMessage::Test(TestKind::Backtest),
                    "One run through the span, which can be played back bar by bar.",
                ),
                chip_button(
                    "Monte Carlo",
                    settings.test == TestKind::MonteCarlo,
                    PanelMessage::Test(TestKind::MonteCarlo),
                    "Draws the backtest's trades at random many times, showing the range of results and drawdowns those trades could have given.",
                ),
                chip_button(
                    "Random blocks",
                    settings.test == TestKind::RandomBlocks,
                    PanelMessage::Test(TestKind::RandomBlocks),
                    "Backtests random stretches of the span, showing whether the strategy works throughout or only in one period.",
                ),
                chip_button(
                    "Optimize",
                    settings.test == TestKind::Optimize,
                    PanelMessage::Test(TestKind::Optimize),
                    "Tries a range of one or two of the strategy's variables and shows which values do best and how steady they are.",
                ),
            ]
            .spacing(2)
        )
        .align_right(Fill),
    ]
    .spacing(7)
    .align_y(Alignment::Center);
    let boxed = |label: &'a str,
                 field: Field,
                 suffix: &'a str,
                 what: &'a str|
     -> Element<'a, PanelMessage> {
        tip(
            row![
                text(label).size(SMALL).color(theme::TEXT_BRIGHT),
                input(field, 54.0),
                unit(suffix),
            ]
            .spacing(5)
            .align_y(Alignment::Center),
            what,
            None,
        )
    };
    let action = |label: &'a str, lit: bool, message: Option<PanelMessage>, what: String| {
        let lit = lit && message.is_some();
        tip(
            button(text(label).size(SMALL).center())
                .padding([3, 9])
                .on_press_maybe(message)
                .style(move |_, status| chip(lit, status)),
            what,
            None,
        )
    };
    // Why the test can't run, shown beside its Run and on it.
    let problem = match (&info.problem, settings.test, &panel.tests.monte_carlo) {
        (Some(problem), _, _) => Some(problem.clone()),
        (None, TestKind::MonteCarlo, Some(Err(error))) => Some(Problem::new(error.clone())),
        _ => None,
    };
    // A value typed but not applied might fix it: Run applies it, and says if it's no good.
    let typed = panel.inputs.iter().any(|(f, _)| f.belongs_to_tests());
    let can_run = info.problem.is_none() || typed;
    let run_or = |what: &str| {
        problem
            .as_ref()
            .map_or_else(|| what.to_owned(), |p| p.message.clone())
    };
    let length = |label: &'a str, value: BlockLength, what: &'a str| {
        chip_button(
            label,
            settings.block_length == value,
            PanelMessage::BlockLength(value),
            what,
        )
    };
    let lengths = || {
        row![
            length("1w", BlockLength::Week, "Blocks of one week."),
            length("1m", BlockLength::Month, "Blocks of 30 days."),
            length("3m", BlockLength::Quarter, "Blocks of 91 days."),
        ]
        .spacing(2)
    };
    let blocks_box = || {
        boxed(
            "Blocks",
            Field::Blocks,
            "",
            "How many random stretches to backtest; they can overlap. Press Enter to apply.",
        )
    };
    let test_settings: Option<Element<'a, PanelMessage>> = match settings.test {
        TestKind::Backtest => None,
        TestKind::MonteCarlo => Some(
            row![
                boxed(
                    "Runs",
                    Field::Runs,
                    "",
                    "How many times to draw the trades: more gives steadier figures. Press Enter to apply.",
                ),
                boxed(
                    "Ruin at",
                    Field::Ruin,
                    "%",
                    "The fall from a peak that counts as ruin. Press Enter to apply.",
                ),
                container(action(
                    "Run again",
                    false,
                    matches!(panel.tests.monte_carlo, Some(Ok(_))).then_some(PanelMessage::RunAgain),
                    "Draw the trades again with new random numbers. Otherwise the same inputs give the same results.".to_owned(),
                ))
                .align_right(Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into(),
        ),
        TestKind::RandomBlocks => {
            let run = match &panel.tests.blocks {
                BlockRun::Running { .. } => action(
                    "Cancel",
                    false,
                    Some(PanelMessage::StopBlocks),
                    "Stop this batch; what it has found so far is dropped.".to_owned(),
                ),
                BlockRun::Done(_) => action(
                    "Run again",
                    false,
                    can_run.then_some(PanelMessage::RunAgain),
                    run_or("Backtest other random stretches. Otherwise the same inputs give the same results.",
                    ),
                ),
                _ => action(
                    "Run",
                    true,
                    can_run.then_some(PanelMessage::RunBlocks),
                    run_or("Backtest that many random stretches of the span, with this chart's strategy and settings. Orders fill on the chart's bars.",
                    ),
                ),
            };
            Some(
                row![blocks_box(), lengths(), container(run).align_right(Fill)]
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .into(),
            )
        }
        TestKind::Optimize => {
            let chosen = &settings.optimize;
            let small = |label: &'a str, field: Field, what: &'a str| {
                tip(
                    row![
                        text(label).size(HINT).color(theme::TEXT_MUTED),
                        input(field, 46.0)
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                    what,
                    None,
                )
            };
            let variable_row = |slot: usize| {
                let mut options: Vec<String> = Vec::new();
                if slot == 1 {
                    options.push(NO_VARIABLE.to_owned());
                }
                options.extend(info.variables.iter().cloned());
                let selected = chosen.variables[slot]
                    .as_ref()
                    .map_or_else(|| NO_VARIABLE.to_owned(), |v| v.name.clone());
                let list = pick_list(options, Some(selected), move |name| {
                    PanelMessage::Variable(slot, name)
                })
                .text_size(SMALL)
                .padding([3, 6])
                .width(104)
                .style(|_, status| list_style(status))
                .menu_style(|_| list_menu_style());
                let what = if slot == 0 {
                    "The strategy's variable to try a range of."
                } else {
                    "A second variable to try with it, for variables that work together. Every pair is tried, so it takes many more backtests."
                };
                let mut line = row![tip_at(list, what, None, tooltip::Position::Right)]
                    .spacing(6)
                    .align_y(Alignment::Center);
                if chosen.variables[slot].is_some() {
                    line = line
                        .push(small("Min", Field::Min(slot), "The lowest value to try."))
                        .push(small(
                            "Max",
                            Field::Max(slot),
                            "The highest value to try, if a step lands on it.",
                        ))
                        .push(small(
                            "Step",
                            Field::Step(slot),
                            "How far apart the values are. Must be above zero.",
                        ));
                }
                line
            };
            let score = tip_at(
                pick_list(ScoreBy::ALL, Some(chosen.score), PanelMessage::ScoreBy)
                    .text_size(SMALL)
                    .padding([3, 6])
                    .width(128)
                    .style(|_, status| list_style(status))
                    .menu_style(|_| list_menu_style()),
                match chosen.score {
                    ScoreBy::Backtest => "What makes a value better. Now: the return of one backtest, which can reward luck.",
                    ScoreBy::MonteCarlo => "What makes a value better. Now: the low end of what its trades could have returned (19 runs in 20 did better), for survival over peak profit.",
                    ScoreBy::RandomBlocks => "What makes a value better. Now: its median return over the same random stretches of the span, for consistency. Takes the most backtests.",
                },
                None,
                tooltip::Position::Right,
            );
            let keep = tip(
                row![
                    toggler(chosen.hold_out)
                        .size(15)
                        .on_toggle(PanelMessage::HoldOut)
                        .style(switch_style),
                    text("Keep unseen").size(SMALL).color(theme::TEXT_BRIGHT),
                ]
                .spacing(5)
                .align_y(Alignment::Center),
                "Pick values on the span's first 70%, then show how the top 5 did on the last 30%. A value that only fit the past usually falls apart there.",
                None,
            );
            let run = match &panel.tests.optimize {
                OptimizeRun::Running { .. } => action(
                    "Cancel",
                    false,
                    Some(PanelMessage::StopOptimize),
                    "Stop this optimization; what it has found so far is dropped.".to_owned(),
                ),
                OptimizeRun::Done(_) => action(
                    "Run again",
                    false,
                    can_run.then_some(PanelMessage::RunAgain),
                    run_or("Run it again with new random numbers. Otherwise the same inputs give the same results.",
                    ),
                ),
                _ => action(
                    "Run",
                    true,
                    can_run.then_some(PanelMessage::RunOptimize),
                    run_or("Backtest every value in the range. Orders fill on the chart's bars.",
                    ),
                ),
            };
            let mut rows = column![
                variable_row(0),
                variable_row(1),
                row![score, keep, container(run).align_right(Fill)]
                    .spacing(8)
                    .align_y(Alignment::Center),
            ]
            .spacing(5);
            if chosen.score == ScoreBy::RandomBlocks {
                rows = rows.push(
                    row![blocks_box(), lengths()]
                        .spacing(10)
                        .align_y(Alignment::Center),
                );
            }
            if let Some(plan) = &info.plan {
                rows = rows.push(text(plan.clone()).size(HINT).color(theme::TEXT_MUTED));
            }
            Some(rows.into())
        }
    };

    let per_order = matches!(settings.commission.value, Commission::PerOrder(_));
    const CAPITAL: &str =
        "Starting cash, in the market's quote currency. Paper trading starts with it too.";
    const FEES: &str =
        "A percentage of each fill, or a flat amount per order, instead of the market's own fee.";
    const SLIPPAGE: &str = "Every market order fills this many ticks worse.";
    const SPREAD: &str = "A spread assumed for bars and trades, in ticks. Quotes have their own.";
    const LATENCY: &str = "Orders reach the venue this many milliseconds late.";
    const LEVERAGE: &str =
        "A position is worth capital × Size % × leverage. Margin calls aren't simulated.";
    const DIRECTION: &str = "Trade one way only; the other side's signals close positions instead. Paper trading follows it too.";
    const SESSION: &str = "Outside these days and times, signals can close positions but not open them. Paper trading follows it too.";
    let ticks_box = |field: Field, suffix: &'a str, what: &'a str| -> Element<'a, PanelMessage> {
        tip(
            row![input(field, 54.0), unit(suffix)]
                .spacing(3)
                .align_y(Alignment::Center),
            what,
            None,
        )
    };
    let simulation = vec![
        line(
            no_switch(),
            name("Capital", CAPITAL),
            tip(input(Field::Capital, 92.0), CAPITAL, None),
        ),
        line(
            switch(settings.commission.on, Edit::Commission, FEES),
            name("Fees", FEES),
            row![
                choice(
                    "%",
                    !per_order,
                    Edit::PerOrder(false),
                    "A percentage of each fill's value.",
                ),
                choice(
                    "flat",
                    per_order,
                    Edit::PerOrder(true),
                    "The same amount for every order, in the quote currency.",
                ),
                tip(input(Field::Commission, 54.0), FEES, None),
            ]
            .spacing(2)
            .align_y(Alignment::Center)
            .into(),
        ),
        line(
            switch(settings.slippage.on, Edit::Slippage, SLIPPAGE),
            name("Slippage", SLIPPAGE),
            ticks_box(Field::Slippage, "ticks", SLIPPAGE),
        ),
        line(
            switch(settings.spread.on, Edit::Spread, SPREAD),
            name("Spread", SPREAD),
            ticks_box(Field::Spread, "ticks", SPREAD),
        ),
        line(
            switch(settings.latency.on, Edit::Latency, LATENCY),
            name("Latency", LATENCY),
            ticks_box(Field::Latency, "ms", LATENCY),
        ),
        line(
            switch(settings.leverage.on, Edit::Leverage, LEVERAGE),
            name("Leverage", LEVERAGE),
            tip(
                row![unit("1 :"), input(Field::Leverage, 54.0)]
                    .spacing(3)
                    .align_y(Alignment::Center),
                LEVERAGE,
                None,
            ),
        ),
        line(
            no_switch(),
            name(
                "Precision",
                "How finely backtests and their playback follow prices inside a bar.",
            ),
            row![
                choice(
                    "Standard",
                    settings.precision == Precision::Standard,
                    Edit::Precision(Precision::Standard),
                    "Fills at the bars, and shows each bar as it closes.",
                ),
                choice(
                    "High",
                    settings.precision == Precision::High,
                    Edit::Precision(Precision::High),
                    "Fills on ticks or 1-minute bars, and shows slow playback's bars forming. It takes longer to load.",
                ),
            ]
            .spacing(2)
            .into(),
        ),
        line(
            switch(settings.direction.on, Edit::Direction, DIRECTION),
            name("Direction", DIRECTION),
            row![
                choice(
                    "Long",
                    settings.direction.value == OneWay::LongOnly,
                    Edit::OneWay(OneWay::LongOnly),
                    "Long positions only; short signals close them.",
                ),
                choice(
                    "Short",
                    settings.direction.value == OneWay::ShortOnly,
                    Edit::OneWay(OneWay::ShortOnly),
                    "Short positions only; long signals close them.",
                ),
            ]
            .spacing(2)
            .into(),
        ),
    ];

    let session = &settings.session.value;
    let days = row((0..7).map(|day| {
        let names = ["M", "T", "W", "T", "F", "S", "S"];
        let whats = [
            "Open positions on Mondays.",
            "Open positions on Tuesdays.",
            "Open positions on Wednesdays.",
            "Open positions on Thursdays.",
            "Open positions on Fridays.",
            "Open positions on Saturdays.",
            "Open positions on Sundays.",
        ];
        choice(names[day], session.days[day], Edit::Day(day), whats[day])
    }))
    .spacing(2);
    let session_row = row![
        switch(settings.session.on, Edit::Session, SESSION),
        name("Session", SESSION),
        days,
        tip(
            input(Field::SessionFrom, 54.0),
            "When the session starts each day, like 09:30.",
            None,
        ),
        unit("to"),
        tip(
            input(Field::SessionTo, 54.0),
            "When it ends. Earlier than the start runs overnight; the same time is all day.",
            None,
        ),
        tip(
            input(Field::SessionZone, 150.0),
            "The session's time zone: UTC, an offset like -05:00, or a name like America/New_York.",
            None,
        ),
        tip(
            checkbox(session.close_after)
                .label("Close at end")
                .size(14)
                .text_size(SMALL)
                .on_toggle(|on| PanelMessage::Edit(Edit::CloseAfter(on))),
            "Close any open position once the session is over.",
            None,
        ),
    ]
    .spacing(7)
    .align_y(Alignment::Center)
    .wrap();

    // Search to add a plugin; what's added shows as tags, each with a way to take it off.
    let search = combo_box(
        &panel.plugin_search,
        "Add a plugin…",
        None,
        |name: &'static str| PanelMessage::AddPlugin(name.to_owned()),
    )
    .size(SMALL)
    .padding([3, 6])
    .width(160)
    .menu_height(150)
    .input_style(|_, status| input_style(status))
    .menu_style(|_| list_menu_style());
    let mut plugin_row = row![tip_at(
        search,
        "Search the kinds of backtest plugin and add one to this chart's backtests.",
        None,
        tooltip::Position::Right,
    )]
    .spacing(6)
    .align_y(Alignment::Center);
    for plugin in &settings.plugins {
        let what = PLUGIN_SLOTS
            .iter()
            .find(|(slot, _)| slot == plugin)
            .map_or("", |(_, what)| *what);
        let tag = button(
            row![
                text(plugin.as_str()).size(SMALL),
                text("×").size(SMALL + 1.0),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([3, 9])
        .on_press(PanelMessage::RemovePlugin(plugin.clone()))
        .style(|_, status| tag_style(status));
        plugin_row = plugin_row.push(tip(tag, what, None));
    }
    let plugin_row = plugin_row
        .push(tip(
            unit("not built yet: no effect"),
            "These kinds of plugin are planned. Adding one is remembered, but changes nothing yet.",
            None,
        ))
        .wrap();

    // As many columns as fit in the panel's left side.
    let side_width = info.width * f32::from(SETTINGS_PORTION) / 10.0;
    let columns = ((side_width / 200.0) as usize).clamp(1, 3);
    let mut body = column![top, tests].spacing(7);
    if let Some(test_settings) = test_settings {
        body = body.push(test_settings);
    }
    if let Some(problem) = &problem {
        let mut line = row![
            text(problem.message.clone())
                .size(SMALL)
                .color(theme::DOWN)
                .width(Fill)
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if problem.widen {
            line = line.push(tip(
                button(text("Widen span").size(SMALL).center())
                    .padding([3, 9])
                    .on_press(PanelMessage::WidenSpan)
                    .style(|_, status| chip(true, status)),
                "Move the span's start back, far enough for these blocks.",
                None,
            ));
        }
        body = body.push(line);
    }
    if let Some(error) = &panel.error {
        body = body.push(text(error.as_str()).size(SMALL).color(theme::DOWN));
    }
    body = body.push(stock_title);
    if open {
        body = body.push(grid(simulation, columns)).push(session_row);
    }
    body = body.push(plugin_row);

    // The panel is kept tall enough for what the sensor measures, so there's nothing to scroll.
    // The scrollable is what lets it measure the full height, and covers a window too short to
    // fit it all.
    scrollable(
        sensor(container(body).width(Fill).padding(Padding {
            top: 10.0,
            right: 14.0,
            bottom: 10.0,
            left: 12.0,
        }))
        .on_resize(|size| PanelMessage::SettingsHeight(size.height)),
    )
    .direction(scrollable::Direction::Vertical(
        scrollable::Scrollbar::hidden(),
    ))
    .width(Fill)
    .height(Fill)
    .into()
}

/// The best of an optimization, each a click from being on the chart: its values, its score, and
/// how it did on bars it wasn't picked on.
fn results(
    found: &OptimizeResult,
    current: Option<usize>,
    applied: bool,
) -> Element<'_, PanelMessage> {
    let grid = &found.grid;
    let cell = |content: String, color: Color, width: f32| {
        text(content)
            .size(HINT)
            .font(iced::Font::MONOSPACE)
            .color(color)
            .width(width)
    };
    let names = match &grid.second {
        Some(second) => format!("{} / {}", grid.first.name, second.name),
        None => grid.first.name.clone(),
    };
    let up_or_down = |value: f64| if value >= 0.0 { theme::UP } else { theme::DOWN };
    let percent = |value: f64| format!("{:+.2}%", value * 100.0);
    let mut list = column![
        row![
            text(names).size(HINT).color(theme::TEXT_MUTED).width(Fill),
            tip(
                cell("Score".to_owned(), theme::TEXT_MUTED, RESULT_FIGURE),
                "This value's own score.",
                None,
            ),
            tip(
                cell("Unseen".to_owned(), theme::TEXT_MUTED, RESULT_FIGURE),
                if found.held_out {
                    "Its return on the last 30% of the span, which it wasn't picked on."
                } else {
                    "Turn on Keep unseen to check the best on bars they weren't picked on."
                },
                None,
            ),
        ]
        .spacing(4)
        .padding([0, 6])
    ]
    .spacing(2)
    .padding(Padding {
        top: 8.0,
        right: 8.0,
        bottom: 6.0,
        left: 8.0,
    })
    .width(RESULTS_WIDTH)
    .height(Fill);
    for ranked in &found.top {
        let unseen = ranked.unseen.map_or_else(
            || cell("–".to_owned(), theme::TEXT_MUTED, RESULT_FIGURE),
            |unseen| {
                let color = if ranked.fell_apart() {
                    FELL_APART
                } else {
                    up_or_down(unseen)
                };
                cell(percent(unseen), color, RESULT_FIGURE)
            },
        );
        let what = match &found.outcomes[ranked.cell] {
            Outcome::Scored { what, .. } => what.as_str(),
            _ => "",
        };
        let warning = if ranked.fell_apart() {
            " It made money where it was picked, but lost on bars it hadn't seen: likely only fit to the past."
        } else {
            ""
        };
        let row = button(
            row![
                text(grid.label(ranked.cell))
                    .size(SMALL)
                    .font(iced::Font::MONOSPACE)
                    .width(Fill),
                cell(
                    percent(ranked.score),
                    up_or_down(ranked.score),
                    RESULT_FIGURE
                ),
                unseen,
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([2, 6])
        .width(Fill)
        .on_press(PanelMessage::Apply(ranked.cell))
        .style(move |_, status| result_style(current == Some(ranked.cell), status));
        // To the left, so it doesn't cover the rows below.
        list = list.push(tip_at(
            row,
            format!(
                "{what} With its neighbours, {}.{warning} Click to put it on the chart's strategy.",
                percent(ranked.plateau)
            ),
            None,
            tooltip::Position::Left,
        ));
    }
    list = list.push(Space::new().height(Fill));
    if grid.second.is_some() {
        let scores = found.outcomes.iter().filter_map(Outcome::score);
        let (low, high) = scores.fold((f64::MAX, f64::MIN), |(lo, hi), s| (lo.min(s), hi.max(s)));
        if low <= high {
            list = list.push(canvas(HeatScale { low, high }).width(Fill).height(26));
        }
    }
    if applied {
        list = list.push(tip(
            button(text("Back to the original").size(SMALL).center())
                .width(Fill)
                .padding([3, 6])
                .on_press(PanelMessage::Revert)
                .style(|_, status| chip(false, status)),
            "Put the strategy's values back to what they were before trying these.",
            None,
        ));
    }
    let divider = container(Space::new())
        .width(1)
        .height(Fill)
        .style(|_| container::background(theme::AXIS_LINE));
    row![divider, list].into()
}

fn result_style(current: bool, status: button::Status) -> button::Style {
    let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: (current || hot).then_some(Background::Color(HOVER)),
        text_color: theme::TEXT_BRIGHT,
        border: Border {
            color: if current {
                theme::ACCENT
            } else {
                Color::TRANSPARENT
            },
            width: 1.0,
            radius: 3.0.into(),
        },
        ..button::Style::default()
    }
}

/// The list beside a fan, under the legend's first line, with its way back from zooming.
fn fan_list<'a>(
    verdict: String,
    rows: Element<'a, PanelMessage>,
    zoomed: bool,
) -> Element<'a, PanelMessage> {
    let mut list = column![
        text(verdict).size(HINT).color(theme::TEXT_BRIGHT),
        container(rows).height(Fill)
    ]
    .spacing(6)
    .padding(Padding {
        top: 8.0,
        right: 8.0,
        bottom: 6.0,
        left: 8.0,
    })
    .width(RESULTS_WIDTH + 30.0)
    .height(Fill);
    if zoomed {
        list = list.push(
            button(text("Show it all").size(SMALL).center())
                .width(Fill)
                .padding([3, 6])
                .on_press(PanelMessage::ZoomFan(None))
                .style(|_, status| chip(false, status)),
        );
    }
    let divider = container(Space::new())
        .width(1)
        .height(Fill)
        .style(|_| container::background(theme::AXIS_LINE));
    row![divider, list].into()
}

/// One line's row in a fan's list: clicking it picks the line, or unpicks it.
fn line_row<'a>(
    label: String,
    color: Color,
    stats: LineStats,
    line: usize,
    picked: Option<usize>,
    what: String,
) -> Element<'a, PanelMessage> {
    let up_or_down = if stats.return_ >= 0.0 {
        theme::UP
    } else {
        theme::DOWN
    };
    let number = |content: String, color: Color| {
        text(content)
            .size(HINT)
            .font(iced::Font::MONOSPACE)
            .color(color)
            .width(RESULT_FIGURE)
    };
    let selected = picked == Some(line);
    tip_at(
        button(
            row![
                text(label).size(HINT).color(color).width(Fill),
                number(format!("{:+.2}%", stats.return_ * 100.0), up_or_down),
                number(format!("{:.2}%", stats.drawdown * 100.0), theme::TEXT),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([2, 6])
        .width(Fill)
        .on_press(PanelMessage::SelectLine((!selected).then_some(line)))
        .style(move |_, status| result_style(selected, status)),
        what,
        None,
        tooltip::Position::Left,
    )
}

/// The heading of a fan's list.
fn list_titles<'a>(first: &'static str) -> Element<'a, PanelMessage> {
    let title = |content: &'static str, width: iced::Length| {
        text(content)
            .size(HINT)
            .color(theme::TEXT_MUTED)
            .width(width)
    };
    row![
        title(first, Fill),
        title("Return", RESULT_FIGURE.into()),
        title("Fell", RESULT_FIGURE.into()),
    ]
    .spacing(4)
    .padding([0, 6])
    .into()
}

/// Monte Carlo's notable runs, and what they come to.
fn monte_carlo_list<'a>(
    result: &MonteCarlo,
    picked: Option<usize>,
    zoomed: bool,
) -> Element<'a, PanelMessage> {
    let verdict = if result.low > 0.0 {
        "Even unlucky draws made money, so these trades look solid."
    } else if result.median > 0.0 {
        "Most draws made money, but an unlucky mix of these trades could lose."
    } else {
        "Most draws lost money, so these trades show no edge here."
    };
    let verdict = if result.ruin > 0.0 {
        format!(
            "{verdict} {:.1}% of draws were ruined.",
            result.ruin * 100.0
        )
    } else {
        verdict.to_owned()
    };
    let rows = column![
        list_titles("Run"),
        line_row(
            "Best".to_owned(),
            theme::UP,
            result.stats[result.best],
            result.best,
            picked,
            "The luckiest draw of all the runs: a ceiling, not something to expect.".to_owned(),
        ),
        line_row(
            "Middle".to_owned(),
            theme::TEXT_BRIGHT,
            result.stats[result.middle],
            result.middle,
            picked,
            "The draw closest to the median: half did better, half worse. The white line is the median at every step.".to_owned(),
        ),
        line_row(
            "Worst".to_owned(),
            theme::DOWN,
            result.stats[result.worst],
            result.worst,
            picked,
            "The unluckiest draw of all the runs: what these same trades could do.".to_owned(),
        ),
        line_row(
            "The backtest".to_owned(),
            Color::from_rgb8(0x42, 0xa5, 0xf5),
            result.actual_stats,
            result.paths.len(),
            picked,
            "The trades as they really came, the bright blue line. Compare where it sits among the draws: near the top means the real order was lucky.".to_owned(),
        ),
    ]
    .spacing(2);
    fan_list(verdict, rows.into(), zoomed)
}

/// Every block, best first.
fn blocks_list<'a>(
    summary: &BlockSummary,
    picked: Option<usize>,
    zoomed: bool,
) -> Element<'a, PanelMessage> {
    let verdict = if summary.median > 0.0 && summary.profitable >= 0.6 {
        "It made money in most stretches of the market."
    } else if summary.profitable <= 0.4 {
        "It lost or stood still in most stretches, so a good backtest may rest on one period."
    } else {
        "It's mixed: it worked in some stretches and not others."
    };
    let mut order: Vec<usize> = (0..summary.blocks.len()).collect();
    order.sort_by(|&a, &b| {
        summary.blocks[b]
            .stats
            .return_
            .total_cmp(&summary.blocks[a].stats.return_)
    });
    let mut rows = column![].spacing(2);
    for line in order {
        let block = &summary.blocks[line];
        let from = DateTime::from_nanos(block.from);
        let to = DateTime::from_nanos(block.to);
        rows = rows.push(line_row(
            format!(
                "{} {:02} – {} {:02} {}",
                from.month_name(),
                from.day,
                to.month_name(),
                to.day,
                to.year
            ),
            theme::TEXT,
            block.stats,
            line,
            picked,
            format!(
                "{} fills. Click to pick out its line and move the chart to it.",
                block.fills
            ),
        ));
    }
    let rows = column![
        list_titles("Block"),
        scrollable(rows)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::hidden()
            ))
            .height(Fill)
    ]
    .spacing(2);
    fan_list(verdict.to_owned(), rows.into(), zoomed)
}

/// Room over the calendar for the legend and the way back up.
const CALENDAR_TOP: f32 = 60.0;
const LEDGER_WIDTH: f32 = 500.0;
const CALENDAR_LABEL: f32 = 34.0;
const CALENDAR_TOTAL: f32 = 66.0;
const CALENDAR_ROW: f32 = 28.0;
const COLUMN_OPENED: f32 = 86.0;
const COLUMN_SIDE: f32 = 38.0;
const COLUMN_PRICE: f32 = 70.0;
const COLUMN_HELD: f32 = 50.0;
const COLUMN_PROFIT: f32 = 66.0;
const COLUMN_PERCENT: f32 = 40.0;
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `+1,234` or `-56.78`: no cents once they stop mattering.
fn money(value: f64) -> String {
    let sign = if value < 0.0 { '-' } else { '+' };
    let size = value.abs();
    if size < 1_000.0 {
        return format!("{sign}{size:.2}");
    }
    let digits = format!("{size:.0}");
    let mut out = String::from(sign);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// [`money`] for a narrow square: whole units once they're enough to read.
fn short_money(value: f64) -> String {
    if value.abs() >= 10.0 {
        format!("{value:+.0}")
    } else {
        money(value)
    }
}

/// Where the calendar is, with a way back up.
fn calendar_crumbs<'a>(level: Level) -> Element<'a, PanelMessage> {
    let back = |label: String, to: Level| {
        button(text(label).size(HINT).center())
            .padding([2, 7])
            .on_press(PanelMessage::CalendarLevel(to))
            .style(|_, status| chip(false, status))
    };
    let here = |content: String| {
        text(content)
            .size(theme::TEXT_SIZE)
            .color(theme::TEXT_BRIGHT)
    };
    let hint = |content: &'static str| {
        text(content)
            .size(theme::TEXT_SIZE)
            .color(theme::TEXT_MUTED)
    };
    let month_name = |month: u32| MONTH_NAMES[month as usize - 1];
    let line = match level {
        Level::Years => row![hint(
            "Profit by the month each trade closed, in UTC. Click a month to see its days."
        )],
        Level::Month(year, month) => row![
            back("‹ All years".to_owned(), Level::Years),
            here(format!("{} {year}", month_name(month))),
            hint("Click a day to see its trades."),
        ],
        Level::Day(year, month, day) => row![
            back(
                format!("‹ {} {year}", month_name(month)),
                Level::Month(year, month),
            ),
            here(format!("{day} {} {year}", month_name(month))),
            hint("Click a trade to replay it on the chart, then press Play."),
        ],
    };
    line.spacing(8).align_y(Alignment::Center).into()
}

/// A square of the calendar, shaded by what it made; clickable if it has trades to open.
fn calendar_cell<'a>(
    corner: Option<String>,
    total: Total,
    reach: f64,
    selected: bool,
    narrow: bool,
    capital: f64,
    open: Option<Level>,
) -> Element<'a, PanelMessage> {
    let mut content = row![].spacing(4).align_y(Alignment::Center);
    if let Some(corner) = corner {
        // Brighter on a shaded square, where the muted color would fade into it.
        let color = if total.trades > 0 {
            theme::TEXT_BRIGHT
        } else {
            theme::TEXT_MUTED
        };
        content = content.push(text(corner).size(HINT).color(color));
    }
    if total.trades > 0 {
        content = content.push(
            text(if narrow {
                short_money(total.profit)
            } else {
                money(total.profit)
            })
            .size(HINT)
            .font(iced::Font::MONOSPACE)
            .color(theme::TEXT_BRIGHT)
            .width(Fill)
            .center(),
        );
    }
    let fill = (total.trades > 0).then(|| heat(total.profit, reach));
    let what = format!(
        "{} ({:+.2}% of the capital), from {} {}",
        money(total.profit),
        total.profit / capital * 100.0,
        total.trades,
        if total.trades == 1 { "trade" } else { "trades" }
    );
    let cell = button(container(content).center_y(Fill).width(Fill))
        .padding([0, 5])
        .width(Fill)
        .height(Fill)
        .on_press_maybe(open.map(PanelMessage::CalendarLevel))
        .style(move |_, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: fill.map(Background::Color),
                text_color: theme::TEXT_BRIGHT,
                border: Border {
                    color: if selected {
                        theme::ACCENT
                    } else if hot {
                        theme::TEXT_MUTED
                    } else {
                        PANEL_BORDER
                    },
                    width: if selected { 1.5 } else { 1.0 },
                    radius: 2.0.into(),
                },
                ..button::Style::default()
            }
        });
    if total.trades > 0 {
        tip(cell, what, None)
    } else {
        cell.into()
    }
}

/// The calendar's squares, and a day's trades beside them.
fn calendar_pane<'a>(
    calendar: &Calendar,
    level: Level,
    replaying: Option<usize>,
    capital: f64,
) -> Element<'a, PanelMessage> {
    let heading = |content: String, width: iced::Length| {
        text(content)
            .size(HINT)
            .color(theme::TEXT_MUTED)
            .width(width)
            .center()
    };
    let label = |content: String| {
        container(text(content).size(HINT).color(theme::TEXT_MUTED))
            .center_y(Fill)
            .width(CALENDAR_LABEL)
    };
    let grid: Element<'a, PanelMessage> = match level {
        Level::Years => {
            let years = calendar.years();
            if years.is_empty() {
                return container(
                    text("This backtest made no trades.")
                        .size(theme::TEXT_SIZE)
                        .color(theme::TEXT_MUTED),
                )
                .padding(Padding {
                    top: CALENDAR_TOP,
                    left: 12.0,
                    ..Padding::ZERO
                })
                .width(Fill)
                .height(Fill)
                .into();
            }
            let reach = years
                .iter()
                .flat_map(|&year| (1..=12).map(move |month| (year, month)))
                .fold(0.0, |most: f64, (year, month)| {
                    most.max(calendar.month(year, month).profit.abs())
                });
            let mut titles = row![Space::new().width(CALENDAR_LABEL)].spacing(2);
            for name in MONTH_NAMES {
                titles = titles.push(heading(name.to_owned(), Fill));
            }
            titles = titles.push(heading("Total".to_owned(), CALENDAR_TOTAL.into()));
            let mut rows = column![].spacing(2);
            for &year in &years {
                let mut line = row![label(year.to_string())]
                    .spacing(2)
                    .height(CALENDAR_ROW);
                for month in 1..=12 {
                    let total = calendar.month(year, month);
                    let open = (total.trades > 0).then_some(Level::Month(year, month));
                    line = line.push(calendar_cell(
                        None, total, reach, false, false, capital, open,
                    ));
                }
                line = line.push(total_text(calendar.year(year), capital));
                rows = rows.push(line);
            }
            column![
                titles,
                scrollable(rows)
                    .direction(scrollable::Direction::Vertical(
                        scrollable::Scrollbar::hidden()
                    ))
                    .height(Fill)
            ]
            .spacing(3)
            .into()
        }
        Level::Month(year, month) | Level::Day(year, month, _) => {
            let selected = match level {
                Level::Day(_, _, day) => Some(day),
                _ => None,
            };
            let reach = (1..=days_in_month(year, month)).fold(0.0, |most: f64, day| {
                most.max(calendar.day(year, month, day).profit.abs())
            });
            let mut titles = row![].spacing(2);
            for name in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
                titles = titles.push(heading(name.to_owned(), Fill));
            }
            titles = titles.push(heading("Week".to_owned(), CALENDAR_TOTAL.into()));
            let mut rows = column![titles].spacing(2);
            for week in weeks(year, month) {
                let mut line = row![].spacing(2).height(Fill);
                let mut sum = Total::default();
                for day in week {
                    line = line.push(match day {
                        Some(day) => {
                            let total = calendar.day(year, month, day);
                            sum.profit += total.profit;
                            sum.trades += total.trades;
                            let open = (total.trades > 0).then_some(Level::Day(year, month, day));
                            calendar_cell(
                                Some(day.to_string()),
                                total,
                                reach,
                                selected == Some(day),
                                selected.is_some(),
                                capital,
                                open,
                            )
                        }
                        None => Element::from(Space::new().width(Fill).height(Fill)),
                    });
                }
                rows = rows.push(line.push(total_text(sum, capital)));
            }
            rows.into()
        }
    };
    let grid = container(grid)
        .padding(Padding {
            top: CALENDAR_TOP,
            right: 10.0,
            bottom: 8.0,
            left: 12.0,
        })
        .width(Fill)
        .height(Fill);
    if let Level::Day(year, month, day) = level {
        row![
            grid,
            ledger(calendar, (year, month, day), replaying, capital)
        ]
        .into()
    } else {
        grid.into()
    }
}

/// A row or week's total, beside its squares.
fn total_text<'a>(total: Total, capital: f64) -> Element<'a, PanelMessage> {
    let color = if total.profit >= 0.0 {
        theme::UP
    } else {
        theme::DOWN
    };
    let content = if total.trades > 0 {
        money(total.profit)
    } else {
        String::new()
    };
    let shown = container(
        text(content)
            .size(HINT)
            .font(iced::Font::MONOSPACE)
            .color(color),
    )
    .center_y(Fill)
    .align_right(CALENDAR_TOTAL);
    if total.trades > 0 {
        tip(
            shown,
            format!("{:+.2}% of the capital", total.profit / capital * 100.0),
            None,
        )
    } else {
        shown.into()
    }
}

/// How long a trade was open: `45m`, `3h 20m`, `2d 4h`.
fn held_for(nanos: UnixNanos) -> String {
    let minutes = nanos / 60_000_000_000;
    match (minutes / 1_440, (minutes / 60) % 24, minutes % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// A price with as many decimals as its size needs.
fn price_text(price: f64) -> String {
    if price >= 1.0 {
        format!("{price:.2}")
    } else {
        format!("{price:.6}")
    }
}

/// A day's trades, each a click from being replayed on the chart.
fn ledger<'a>(
    calendar: &Calendar,
    (year, month, day): (i64, u32, u32),
    replaying: Option<usize>,
    capital: f64,
) -> Element<'a, PanelMessage> {
    let cell = |content: String, color: Color, width: f32| {
        text(content)
            .size(HINT)
            .font(iced::Font::MONOSPACE)
            .color(color)
            .width(width)
    };
    let muted = |content: &str, width: f32| cell(content.to_owned(), theme::TEXT_MUTED, width);
    let trades = calendar.trades_on(year, month, day);
    let step = |label: &'static str, to: Option<Level>, what: &'static str| {
        tip(
            button(text(label).size(HINT).center())
                .padding([2, 7])
                .on_press_maybe(to.map(PanelMessage::CalendarLevel))
                .style(|_, status| chip(false, status)),
            what,
            None,
        )
    };
    let total = calendar.day(year, month, day);
    let color = if total.profit >= 0.0 {
        theme::UP
    } else {
        theme::DOWN
    };
    let stepping = row![
        step(
            "‹ Day before",
            calendar.day_before(year, month, day),
            "The nearest earlier day with trades.",
        ),
        container(
            row![
                text(format!("{day} {} {year}", MONTH_NAMES[month as usize - 1]))
                    .size(SMALL)
                    .color(theme::TEXT_BRIGHT),
                text(format!(
                    "{} ({:+.2}%)",
                    money(total.profit),
                    total.profit / capital * 100.0
                ))
                .size(HINT)
                .font(iced::Font::MONOSPACE)
                .color(color),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .center_x(Fill),
        step(
            "Day after ›",
            calendar.day_after(year, month, day),
            "The nearest later day with trades.",
        ),
    ]
    .align_y(Alignment::Center);
    let titles = row![
        muted("Opened", COLUMN_OPENED),
        muted("Side", COLUMN_SIDE),
        muted("Entry", COLUMN_PRICE),
        muted("Exit", COLUMN_PRICE),
        muted("Held", COLUMN_HELD),
        muted("Profit", COLUMN_PROFIT),
        muted("%", COLUMN_PERCENT),
    ]
    .spacing(4)
    .padding([0, 6]);
    let mut rows = column![].spacing(2);
    for &index in trades {
        let trade: &RoundTrip = &calendar.trades[index];
        let color = if trade.profit >= 0.0 {
            theme::UP
        } else {
            theme::DOWN
        };
        let opened = DateTime::from_nanos(trade.entry_ts);
        let (exit, held) = if trade.open {
            ("open".to_owned(), "–".to_owned())
        } else {
            (
                price_text(trade.exit_price),
                held_for(trade.exit_ts.saturating_sub(trade.entry_ts)),
            )
        };
        let line = button(
            row![
                cell(
                    format!("{:02}/{:02} {}", opened.month, opened.day, opened.time()),
                    theme::TEXT,
                    COLUMN_OPENED,
                ),
                cell(
                    if trade.long { "Long" } else { "Short" }.to_owned(),
                    if trade.long { theme::UP } else { theme::DOWN },
                    COLUMN_SIDE,
                ),
                cell(price_text(trade.entry_price), theme::TEXT, COLUMN_PRICE),
                cell(exit, theme::TEXT, COLUMN_PRICE),
                cell(held, theme::TEXT, COLUMN_HELD),
                cell(money(trade.profit), color, COLUMN_PROFIT),
                cell(
                    format!("{:+.2}", trade.profit / capital * 100.0),
                    color,
                    COLUMN_PERCENT,
                ),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([3, 6])
        .width(Fill)
        .on_press(PanelMessage::ReplayTrade(index))
        .style(move |_, status| result_style(replaying == Some(index), status));
        let still_open = if trade.open {
            " Still open at the end of the backtest, so its profit is at the last close."
        } else {
            ""
        };
        rows = rows.push(tip_at(
            line,
            format!(
                "Size {:.4}, fees {:.2}.{still_open} Click to move the chart to just before it opened, then press Play to watch it.",
                trade.qty, trade.fees,
            ),
            None,
            tooltip::Position::Left,
        ));
    }
    let list = column![
        stepping,
        titles,
        scrollable(rows)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::hidden()
            ))
            .height(Fill)
    ]
    .spacing(4)
    .padding(Padding {
        // Under the legend's first line, which runs across the pane.
        top: 36.0,
        right: 8.0,
        bottom: 6.0,
        left: 8.0,
    })
    .width(LEDGER_WIDTH)
    .height(Fill);
    let divider = container(Space::new())
        .width(1)
        .height(Fill)
        .style(|_| container::background(theme::AXIS_LINE));
    row![divider, list].into()
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
