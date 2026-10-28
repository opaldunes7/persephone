//! The panel and its settings sheet, laid out.

use iced::{
    Alignment, Background, Border, Color, Element, Fill, Length, Padding,
    widget::{
        Space, button, canvas, checkbox, column, container, row, rule, scrollable, text,
        text_input, toggler,
    },
};
use persephone_chart::theme;
use persephone_core::{UnixNanos, time::DateTime};
use persephone_engine::BacktestReport;

use super::{
    Commission, Edit, Field, OneWay, Panel, PanelMessage, Precision, Settings, progress,
    speed_text,
    widgets::{Edge, Equity, Span, Speed},
};

/// How wide the settings sheet is.
pub const SETTINGS_WIDTH: f32 = 390.0;
/// The column of controls and statistics left of the equity curve.
const SIDE_WIDTH: f32 = 236.0;
const ROW_HEIGHT: f32 = 30.0;

const PANEL: Color = Color::from_rgb8(0x1e, 0x22, 0x2d);
const PANEL_BORDER: Color = Color::from_rgb8(0x36, 0x3a, 0x45);
const HOVER: Color = Color::from_rgb8(0x2a, 0x2e, 0x39);

/// Where the chart's backtest stands.
pub enum Status<'a> {
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
    /// Bars closed at the playhead, while there is one.
    pub shown: Option<usize>,
    /// A bar's usual length, for the speed's bars-per-second.
    pub bar_nanos: UnixNanos,
    /// How orders fill, when worth saying.
    pub fills: Option<String>,
}

pub fn panel(info: PanelInfo<'_>) -> Element<'_, PanelMessage> {
    let panel = info.panel;
    let playing = panel.playback.playing;
    let control = |label: &'static str, tip: PanelMessage, enabled: bool| {
        button(text(label).size(13).center())
            .width(30)
            .height(24)
            .padding(0)
            .on_press_maybe(enabled.then_some(tip))
            .style(|_, status| chip(false, status))
    };
    let ready = matches!(info.status, Status::Done(_));
    let controls = row![
        if playing {
            button(text("❚❚  Pause").size(12).center())
                .width(78)
                .height(24)
                .padding(0)
                .on_press(PanelMessage::Pause)
                .style(|_, status| chip(true, status))
        } else {
            button(text("▶  Start").size(12).center())
                .width(78)
                .height(24)
                .padding(0)
                .on_press_maybe(ready.then_some(PanelMessage::Play))
                .style(|_, status| chip(true, status))
        },
        control(
            "⟲",
            PanelMessage::Restart,
            ready && panel.playback.at.is_some()
        ),
        button(text("Step").size(12).center())
            .width(48)
            .height(24)
            .padding(0)
            .on_press_maybe(ready.then_some(PanelMessage::Step))
            .style(|_, status| chip(false, status)),
    ]
    .spacing(4);

    let muted = |content: String| text(content).size(11).color(theme::TEXT_MUTED);
    let mut side = column![
        text(info.title.clone()).size(12).color(theme::TEXT_BRIGHT),
        controls,
    ]
    .spacing(6)
    .width(SIDE_WIDTH);
    let mut equity: Element<'_, PanelMessage> = Space::new().width(Fill).height(Fill).into();
    match &info.status {
        Status::Waiting => side = side.push(muted("Waiting for the chart's bars…".into())),
        Status::Running => side = side.push(muted("Backtesting…".into())),
        Status::Failed(message) => {
            side = side.push(text(*message).size(11).color(theme::DOWN));
        }
        Status::Done(report) => {
            let bars = info.shown.unwrap_or(report.equity.len());
            let now = progress(report, info.from_index, bars);
            let color = if now.return_ >= 0.0 {
                theme::UP
            } else {
                theme::DOWN
            };
            let mono = |content: String, color: Color| {
                text(content)
                    .size(12)
                    .font(iced::Font::MONOSPACE)
                    .color(color)
            };
            side = side
                .push(row![
                    mono(format!("{:+.2}%", now.return_ * 100.0), color),
                    Space::new().width(10),
                    mono(format!("DD {:.1}%", now.drawdown * 100.0), theme::TEXT),
                    Space::new().width(10),
                    mono(format!("{} trades", now.trades), theme::TEXT),
                ])
                .push(muted(format!("Equity {:.2} {}", now.equity, info.quote)));
            let values = report.equity.get(info.from_index..).unwrap_or_default();
            equity = canvas(Equity {
                values,
                shown: bars.saturating_sub(info.from_index),
                start: report.starting_balance,
            })
            .width(Fill)
            .height(Fill)
            .into();
        }
    }
    if let Some(fills) = info.fills {
        side = side.push(muted(fills));
    }

    let corner = column![
        button(text("⚙").size(14).center())
            .width(30)
            .height(24)
            .padding(0)
            .on_press(PanelMessage::ToggleSettings)
            .style(move |_, status| chip(panel.settings_open, status)),
        button(text("✕").size(12).center())
            .width(30)
            .height(24)
            .padding(0)
            .on_press(PanelMessage::Close)
            .style(|_, status| chip(false, status)),
    ]
    .spacing(4);

    let top = row![side, equity, corner].spacing(10).height(Fill);

    // The span row: the slider once the history is known.
    let label = |content: &'static str| {
        text(content)
            .size(11)
            .color(theme::TEXT_MUTED)
            .width(44)
            .align_y(Alignment::Center)
            .height(ROW_HEIGHT)
    };
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
        .height(ROW_HEIGHT - 6.0)
        .into(),
        Some(Err(error)) => text(format!("The history couldn't be loaded: {error}"))
            .size(11)
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
    let latest = button(text("Latest bars").size(11).center())
        .padding([3, 8])
        .on_press_maybe(info.span.is_some().then_some(PanelMessage::Latest))
        .style(|_, status| chip(false, status));
    let span_row = row![
        label("Span"),
        span_widget,
        text(dates)
            .size(11)
            .font(iced::Font::MONOSPACE)
            .color(theme::TEXT)
            .width(190)
            .align_x(Alignment::Center),
        latest,
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .height(ROW_HEIGHT);

    let speed_row = row![
        label("Speed"),
        canvas(Speed {
            position: panel.playback.speed,
        })
        .width(Fill)
        .height(ROW_HEIGHT),
        text(speed_text(panel.playback.speed, info.bar_nanos))
            .size(11)
            .font(iced::Font::MONOSPACE)
            .color(theme::TEXT_BRIGHT)
            .width(274),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .height(ROW_HEIGHT);

    let body = column![top, span_row, speed_row]
        .spacing(4)
        .padding(Padding {
            top: 8.0,
            right: 10.0,
            bottom: 2.0,
            left: 12.0,
        })
        .height(Fill);
    container(column![body, canvas(Edge).width(Fill).height(7)])
        .width(Fill)
        .height(Fill)
        .style(|_| container::background(PANEL))
        .into()
}

/// The settings sheet: stock settings, each with its switch, then the plugin slots.
pub fn settings_sheet<'a>(panel: &'a Panel, settings: &'a Settings) -> Element<'a, PanelMessage> {
    let stock = settings.stock;
    let input = |field: Field, width: Length| {
        let typed = panel
            .input(field)
            .map_or_else(|| settings.text(field), str::to_owned);
        text_input("", &typed)
            .on_input(move |t| PanelMessage::Input(field, t))
            .on_submit(PanelMessage::Submit(field))
            .padding([3, 6])
            .size(12)
            .width(width)
            .style(|_, status| input_style(status))
    };
    let switch = |on: bool, edit: fn(bool) -> Edit| {
        toggler(on)
            .size(16)
            .on_toggle_maybe(stock.then_some(move |on| PanelMessage::Edit(edit(on))))
    };
    let heading = |title: &'a str| text(title).size(11).color(theme::TEXT_MUTED);
    let note = |content: &'a str| text(content).size(11).color(theme::TEXT_MUTED);
    // A row: switch, name, and its own controls on the right.
    let line = |toggle: Option<Element<'a, PanelMessage>>,
                name: &'a str,
                controls: Element<'a, PanelMessage>|
     -> Element<'a, PanelMessage> {
        row![
            toggle.unwrap_or_else(|| Space::new().width(30).into()),
            text(name).size(12).color(theme::TEXT_BRIGHT).width(Fill),
            controls,
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    };
    let choice = |label: &'a str, selected: bool, message: Option<PanelMessage>| {
        button(text(label).size(11).center())
            .padding([2, 8])
            .on_press_maybe(message)
            .style(move |_, status| chip(selected, status))
    };
    let unit = |content: &'a str| text(content).size(11).color(theme::TEXT_MUTED);

    let per_order = matches!(settings.commission.value, Commission::PerOrder(_));
    let session = &settings.session.value;
    let days = row((0..7).map(|day| {
        let names = ["M", "T", "W", "T", "F", "S", "S"];
        choice(
            names[day],
            session.days[day],
            Some(PanelMessage::Edit(Edit::Day(day))),
        )
        .width(26)
        .into()
    }))
    .spacing(3);

    let mut body = column![
        row![
            text("Backtest settings").size(13).color(theme::TEXT_BRIGHT).width(Fill),
            text("Stock settings").size(11).color(theme::TEXT_MUTED),
            toggler(stock)
                .size(16)
                .on_toggle(|on| PanelMessage::Edit(Edit::Stock(on))),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        note("Each switch turns a setting on; Stock settings turns them all off at once, for an ideal run to compare against."),
        rule::horizontal(1),
        heading("SIMULATION · backtests only"),
        line(None, "Starting capital", row![input(Field::Capital, Length::Fixed(110.0))].into()),
        line(
            Some(switch(settings.commission.on, Edit::Commission).into()),
            "Commission",
            row![
                choice("%", !per_order, Some(PanelMessage::Edit(Edit::PerOrder(false)))),
                choice("per order", per_order, Some(PanelMessage::Edit(Edit::PerOrder(true)))),
                input(Field::Commission, Length::Fixed(64.0)),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into(),
        ),
        line(
            Some(switch(settings.slippage.on, Edit::Slippage).into()),
            "Slippage",
            row![input(Field::Slippage, Length::Fixed(56.0)), unit("ticks")].spacing(4).align_y(Alignment::Center).into(),
        ),
        line(
            Some(switch(settings.spread.on, Edit::Spread).into()),
            "Spread",
            row![input(Field::Spread, Length::Fixed(56.0)), unit("ticks")].spacing(4).align_y(Alignment::Center).into(),
        ),
        note("Slippage moves every market order against you. The spread is assumed only for bars and trades; quotes have their own."),
        line(
            Some(switch(settings.latency.on, Edit::Latency).into()),
            "Latency",
            row![input(Field::Latency, Length::Fixed(56.0)), unit("ms")].spacing(4).align_y(Alignment::Center).into(),
        ),
        line(
            Some(switch(settings.leverage.on, Edit::Leverage).into()),
            "Leverage",
            row![unit("1 :"), input(Field::Leverage, Length::Fixed(56.0))].spacing(4).align_y(Alignment::Center).into(),
        ),
        note("A position is worth capital × Size % × leverage. Margin calls aren't simulated."),
        line(
            None,
            "Intra-bar precision",
            row![
                choice(
                    "Standard",
                    settings.precision == Precision::Standard,
                    Some(PanelMessage::Edit(Edit::Precision(Precision::Standard))),
                ),
                choice(
                    "High",
                    settings.precision == Precision::High,
                    Some(PanelMessage::Edit(Edit::Precision(Precision::High))),
                ),
            ]
            .spacing(4)
            .into(),
        ),
        note(match settings.precision {
            Precision::Standard => "Standard: orders fill at the bars, and playback shows each bar as it closes. Fast and light.",
            Precision::High => "High: orders fill on ticks or 1-minute bars, and slow playback shows each bar forming. Takes longer to load.",
        }),
        rule::horizontal(1),
        heading("TRADING RULES · backtests and paper trading"),
        line(
            Some(switch(settings.direction.on, Edit::Direction).into()),
            "Direction",
            row![
                choice(
                    "Long only",
                    settings.direction.value == OneWay::LongOnly,
                    Some(PanelMessage::Edit(Edit::OneWay(OneWay::LongOnly))),
                ),
                choice(
                    "Short only",
                    settings.direction.value == OneWay::ShortOnly,
                    Some(PanelMessage::Edit(Edit::OneWay(OneWay::ShortOnly))),
                ),
            ]
            .spacing(4)
            .into(),
        ),
        line(
            Some(switch(settings.session.on, Edit::Session).into()),
            "Trading session",
            days.into(),
        ),
        row![
            Space::new().width(38),
            input(Field::SessionFrom, Length::Fixed(58.0)),
            unit("to"),
            input(Field::SessionTo, Length::Fixed(58.0)),
            input(Field::SessionZone, Length::Fill),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
        row![
            Space::new().width(38),
            checkbox(session.close_after)
                .label("Close positions when the session ends")
                .size(13)
                .text_size(11)
                .on_toggle(|on| PanelMessage::Edit(Edit::CloseAfter(on))),
        ],
        note("Outside the session, signals can close positions but not open them."),
    ]
    .spacing(7);
    if let Some(error) = &panel.error {
        body = body.push(text(error.as_str()).size(11).color(theme::DOWN));
    }

    body = body
        .push(rule::horizontal(1))
        .push(heading("PLUGINS · coming"))
        .push(note(
            "Models whose maths depends on the market or the broker will be plugins you can write (see docs/plugins.md). None of these exist yet.",
        ));
    for (name, what) in PLUGIN_SLOTS {
        body = body.push(
            column![
                row![
                    text(name).size(12).color(theme::TEXT_BRIGHT).width(Fill),
                    text("None yet").size(11).color(theme::TEXT_MUTED),
                ],
                note(what),
            ]
            .spacing(2),
        );
    }

    container(scrollable(container(body).padding(Padding {
        top: 10.0,
        right: 16.0,
        bottom: 12.0,
        left: 12.0,
    })))
    .width(SETTINGS_WIDTH)
    .style(|_| container::Style {
        background: Some(Background::Color(PANEL)),
        border: Border {
            color: PANEL_BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

/// The kinds of backtest plugin to come, and what each would do.
pub const PLUGIN_SLOTS: [(&str, &str); 7] = [
    (
        "Slippage model",
        "Slippage from the order and the market, e.g. growing fast once an order is more than 5% of the bar's volume.",
    ),
    (
        "Fill model",
        "Order-book or queue simulation: partial fills, or limit orders that fill only once the price trades through them.",
    ),
    (
        "Spread and latency",
        "Spreads and delays that change, e.g. widening by 10 pips and lagging 200 ms when an economic calendar says CPI is out.",
    ),
    (
        "Position sizing",
        "How big each position is: Kelly, volatility or ATR targeting, risk per trade. Without one, the strategy's Size % applies.",
    ),
    (
        "Holding costs",
        "Overnight swaps or funding from your broker's history, charged on positions held.",
    ),
    (
        "Commission schedule",
        "Tiered or broker-specific fees, e.g. cheaper as monthly volume grows.",
    ),
    (
        "Session calendar",
        "Exchange holidays and half days, on top of the trading session.",
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
