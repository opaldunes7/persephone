//! The backtest's calendar: months, days and a day's trades.

use iced::widget::column;

use super::*;

/// Room over the calendar for the legend and the way back up.
pub(super) const CALENDAR_TOP: f32 = 60.0;
pub(super) const LEDGER_WIDTH: f32 = 500.0;
pub(super) const CALENDAR_LABEL: f32 = 34.0;
pub(super) const CALENDAR_TOTAL: f32 = 66.0;
pub(super) const CALENDAR_ROW: f32 = 28.0;
/// The most rows a table of trades shows.
pub(super) const MAX_TRADE_ROWS: usize = 1_000;
pub(super) const COLUMN_DRAW: f32 = 26.0;
pub(super) const COLUMN_EQUITY: f32 = 52.0;
pub(super) const COLUMN_OPENED: f32 = 86.0;
pub(super) const COLUMN_SIDE: f32 = 38.0;
pub(super) const COLUMN_PRICE: f32 = 70.0;
pub(super) const COLUMN_HELD: f32 = 50.0;
pub(super) const COLUMN_PROFIT: f32 = 66.0;
pub(super) const COLUMN_PERCENT: f32 = 40.0;
pub(super) const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `+1,234` or `-56.78`: no cents once they stop mattering.
pub(super) fn money(value: f64) -> String {
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
pub(super) fn short_money(value: f64) -> String {
    if value.abs() >= 10.0 {
        format!("{value:+.0}")
    } else {
        money(value)
    }
}

/// Where the calendar is, with a way back up.
pub(super) fn calendar_crumbs<'a>(level: Level) -> Element<'a, PanelMessage> {
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
pub(super) fn calendar_cell<'a>(
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
pub(super) fn calendar_pane<'a>(
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
pub(super) fn total_text<'a>(total: Total, capital: f64) -> Element<'a, PanelMessage> {
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
pub(super) fn held_for(nanos: UnixNanos) -> String {
    let minutes = nanos / 60_000_000_000;
    match (minutes / 1_440, (minutes / 60) % 24, minutes % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// A price with as many decimals as its size needs.
pub(super) fn price_text(price: f64) -> String {
    if price >= 1.0 {
        format!("{price:.2}")
    } else {
        format!("{price:.6}")
    }
}

/// A day's trades, each a click from being replayed on the chart.
pub(super) fn ledger<'a>(
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
