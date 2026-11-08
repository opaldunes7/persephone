//! A run's trades beside its test, each a click from being followed on the chart.

use iced::widget::column;

use super::*;
use crate::backtest::{Inspected, Inspection};

/// The most rows drawn at once; the rest are said to be there.
const MOST_ROWS: usize = 1_000;

fn muted<'a>(content: String) -> iced::widget::Text<'a> {
    text(content).size(SMALL).color(theme::TEXT_MUTED)
}

/// The drawer: what run it is, how its trades are ordered, and the trades.
pub(super) fn inspect_drawer<'a>(
    inspection: &'a Inspection,
    quote: &'a str,
) -> Element<'a, PanelMessage> {
    let close = tip(
        button(text("✕").size(12).center())
            .padding([0, 6])
            .on_press(PanelMessage::CloseInspection)
            .style(|_, status| plain(status)),
        "Close the trades, and the playback goes back to the chart's own.",
        None,
    );
    let by_profit = inspection.by_profit;
    let order = row![
        tip(
            button(text("In order").size(HINT).center())
                .padding([2, 7])
                .on_press(PanelMessage::InspectByProfit(false))
                .style(move |_, status| chip(!by_profit, status)),
            "In the order they opened.",
            None,
        ),
        tip(
            button(text("By profit").size(HINT).center())
                .padding([2, 7])
                .on_press(PanelMessage::InspectByProfit(true))
                .style(move |_, status| chip(by_profit, status)),
            "Biggest profit first, so the best and worst are a click away.",
            None,
        ),
    ]
    .spacing(2);
    let heading = row![
        text(inspection.title.as_str())
            .size(SMALL)
            .color(theme::TEXT_BRIGHT)
            .width(Fill),
        order,
        close,
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    let body: Element<'a, PanelMessage> = match &inspection.found {
        Inspected::Finding(_) => muted("Running it again to find its trades…".into()).into(),
        Inspected::Failed(error) => text(error.as_str()).size(SMALL).color(theme::DOWN).into(),
        Inspected::Ready(ledger) => trades(inspection, ledger, quote),
    };
    let divider = container(Space::new())
        .width(1)
        .height(Fill)
        .style(|_| container::background(theme::AXIS_LINE));
    row![
        divider,
        column![heading, body]
            .spacing(6)
            .padding(Padding {
                top: 8.0,
                right: 8.0,
                bottom: 6.0,
                left: 8.0,
            })
            .width(Fill)
            .height(Fill),
    ]
    .width(FillPortion(2))
    .height(Fill)
    .into()
}

fn trades<'a>(
    inspection: &'a Inspection,
    ledger: &'a crate::backtest::trades::Ledger,
    quote: &'a str,
) -> Element<'a, PanelMessage> {
    let cell = |value: String, width: f32, color: Color| {
        text(value)
            .size(HINT)
            .width(width)
            .color(color)
            .font(iced::Font::MONOSPACE)
    };
    let many_markets = ledger
        .rows
        .windows(2)
        .any(|pair| pair[0].symbol != pair[1].symbol);
    // Short enough to keep a row on one line: `24-03-10 14:00`.
    let when = |ts: UnixNanos| {
        let at = DateTime::from_nanos(ts);
        let date = at.date();
        format!("{} {}", date.get(2..).unwrap_or(&date), &at.time()[..5])
    };
    let mut header = row![
        cell("Opened".into(), 112.0, theme::TEXT_MUTED),
        cell("Closed".into(), 112.0, theme::TEXT_MUTED),
    ]
    .spacing(4);
    if many_markets {
        header = header.push(cell("Market".into(), 66.0, theme::TEXT_MUTED));
    }
    header = header
        .push(cell("Side".into(), 38.0, theme::TEXT_MUTED))
        .push(cell("Size".into(), 64.0, theme::TEXT_MUTED))
        .push(tip(
            cell("Profit".into(), 66.0, theme::TEXT_MUTED),
            format!("After fees, in {quote}. One still open at the end counts at its last value."),
            None,
        ))
        .push(tip(
            cell("Return".into(), 56.0, theme::TEXT_MUTED),
            "Its profit on what it put in: its size at its entry price.",
            None,
        ))
        .push(cell("Fees".into(), 56.0, theme::TEXT_MUTED));
    let mut rows = column![].spacing(1);
    let order = inspection.order();
    for &at in order.iter().take(MOST_ROWS) {
        let row = &ledger.rows[at];
        let trip = &row.trip;
        let made = if trip.profit >= 0.0 {
            theme::UP
        } else {
            theme::DOWN
        };
        let mut line = row![
            cell(when(trip.entry_ts), 112.0, theme::TEXT),
            cell(
                if trip.open {
                    "still open".to_owned()
                } else {
                    when(trip.exit_ts)
                },
                112.0,
                theme::TEXT
            ),
        ]
        .spacing(4);
        if many_markets {
            line = line.push(cell(row.symbol.clone(), 66.0, theme::TEXT));
        }
        line = line
            .push(cell(
                if trip.long { "Long" } else { "Short" }.into(),
                38.0,
                if trip.long { theme::UP } else { theme::DOWN },
            ))
            .push(cell(format!("{:.4}", trip.qty), 64.0, theme::TEXT))
            .push(cell(format!("{:+.2}", trip.profit), 66.0, made))
            .push(cell(
                format!("{:+.2}%", row.return_on_position * 100.0),
                56.0,
                made,
            ))
            .push(cell(format!("{:.2}", trip.fees), 56.0, theme::TEXT_MUTED));
        let picked = inspection.picked == Some(at);
        rows = rows.push(
            button(line)
                .padding([1, 4])
                .on_press(PanelMessage::InspectTrade(at))
                .style(move |_, status| result_style(picked, status)),
        );
    }
    let total = ledger.rows.len();
    let said = if total == 0 {
        persephone_research::figures::NO_TRADES.to_owned()
    } else if total > MOST_ROWS {
        format!(
            "The first {MOST_ROWS} of {total} trades, {:+.2} {quote} in all. Click one to follow it.",
            ledger.profit()
        )
    } else {
        format!(
            "{total} trades, {:+.2} {quote} in all. Click one to follow it on the chart, then Play.",
            ledger.profit()
        )
    };
    // Down through the trades, and across when the drawer's narrower than a row: the heading
    // goes with the rows, so the columns stay under their names.
    let table = column![container(header).padding([0, 4]), rows].spacing(2);
    column![
        muted(said),
        scrollable(table)
            .direction(scrollable::Direction::Both {
                vertical: scrollable::Scrollbar::default(),
                horizontal: scrollable::Scrollbar::default(),
            })
            .width(Fill)
            .height(Fill),
    ]
    .spacing(4)
    .into()
}
