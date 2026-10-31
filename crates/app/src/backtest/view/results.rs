//! What the tests found: the lists and tables beside their lines, and a run's trades.

use iced::widget::column;

use super::*;

/// The best of an optimization, each a click from being on the chart: its values, its score, and
/// how it did on bars it wasn't picked on.
pub(super) fn results(
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

pub(super) fn result_style(current: bool, status: button::Status) -> button::Style {
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
pub(super) fn fan_list<'a>(
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
pub(super) fn line_row<'a>(
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
        .on_press(PanelMessage::SelectLine(Some(line)))
        .style(move |_, status| result_style(selected, status)),
        what,
        None,
        tooltip::Position::Left,
    )
}

/// The heading of a fan's list.
pub(super) fn list_titles<'a>(first: &'static str) -> Element<'a, PanelMessage> {
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
pub(super) fn monte_carlo_list<'a>(
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

/// A Monte Carlo line's trades, to go beside the lines.
pub(super) fn run_trades<'a>(
    result: &MonteCarlo,
    line: usize,
    trades: &[RoundTrip],
    capital: f64,
    shown: Option<usize>,
) -> Element<'a, PanelMessage> {
    let run = result.run_of_line(line).unwrap_or(0);
    half(monte_carlo_trades(
        result,
        run,
        trades,
        capital,
        shown,
        Some(fan_switches(
            &[
                ("Best", result.best),
                ("Middle", result.middle),
                ("Worst", result.worst),
                ("The backtest", result.paths.len()),
            ],
            line,
            PanelMessage::ShowRuns,
            "Back to Best, Middle and Worst. This line stays picked.",
        )),
    ))
}

/// The way back to a fan's list, and straight to its notable lines.
pub(super) fn fan_switches<'a>(
    lines: &[(&'static str, usize)],
    picked: usize,
    back: PanelMessage,
    back_tip: &'static str,
) -> Element<'a, PanelMessage> {
    let mut switches = row![tip(
        button(text("‹ All").size(HINT).center())
            .padding([2, 7])
            .on_press(back)
            .style(|_, status| chip(false, status)),
        back_tip,
        None,
    )]
    .spacing(2);
    for &(name, line) in lines {
        switches = switches.push(
            button(text(name).size(HINT).center())
                .padding([2, 7])
                .on_press(PanelMessage::SelectLine(Some(line)))
                .style(move |_, status| chip(picked == line, status)),
        );
    }
    switches.into()
}

/// Half of the pane, with a line down its edge, under the legend's first line.
pub(super) fn half(content: Element<'_, PanelMessage>) -> Element<'_, PanelMessage> {
    let divider = container(Space::new())
        .width(1)
        .height(Fill)
        .style(|_| container::background(theme::AXIS_LINE));
    row![
        divider,
        container(content)
            .padding(Padding {
                top: 8.0,
                right: 8.0,
                bottom: 6.0,
                left: 8.0,
            })
            .width(Fill)
            .height(Fill)
    ]
    .width(FillPortion(1))
    .into()
}

/// Lines or a table, for Monte Carlo's pane.
pub(super) fn runs_view_chips<'a>(table: bool) -> Element<'a, PanelMessage> {
    row![
        tip(
            button(text("Lines").size(HINT).center())
                .padding([2, 7])
                .on_press(PanelMessage::ShowRunsTable(false))
                .style(move |_, status| chip(!table, status)),
            "Each run as a line, to see how they spread out. Pick one to see its trades beside them.",
            None,
        ),
        tip(
            button(text("Table").size(HINT).center())
                .padding([2, 7])
                .on_press(PanelMessage::ShowRunsTable(true))
                .style(move |_, status| chip(table, status)),
            "Every run as a row beside the trades of the one picked. Click a column to sort it.",
            None,
        ),
    ]
    .spacing(2)
    .into()
}

/// Half every run in a table, half the trades of the one picked: the top one until then, so
/// neither half is ever empty.
pub(super) fn runs_pane<'a>(
    result: &MonteCarlo,
    picked: Option<usize>,
    sort: (RunColumn, bool),
    scroll: usize,
    trades: &[RoundTrip],
    capital: f64,
    shown: Option<usize>,
) -> Element<'a, PanelMessage> {
    let rows = result.ranked_runs();
    let run = picked.unwrap_or(rows[0].run);
    table_and_trades(
        runs_table(
            rows,
            sort,
            ("Run", Vec::new()),
            capital,
            result.ruin_at,
            scroll,
            run,
        ),
        monte_carlo_trades(result, run, trades, capital, shown, None),
    )
}

/// The same for blocks. Which block's picked is also where the chart goes.
pub(super) fn blocks_pane<'a>(
    summary: &BlockSummary,
    picked: Option<usize>,
    sort: (RunColumn, bool),
    scroll: usize,
    capital: f64,
    shown: Option<usize>,
) -> Element<'a, PanelMessage> {
    let rows = summary.ranked();
    let run = picked
        .or_else(|| rows.first().map(|row| row.run))
        .unwrap_or(0);
    let names = summary.blocks.iter().map(block_name).collect();
    table_and_trades(
        runs_table(
            rows,
            sort,
            ("Block", names),
            capital,
            f64::INFINITY,
            scroll,
            run,
        ),
        block_trades(summary, run, capital, shown, None),
    )
}

/// A block's real trades.
pub(super) fn block_trades<'a>(
    summary: &BlockSummary,
    run: usize,
    capital: f64,
    shown: Option<usize>,
    top: Option<Element<'a, PanelMessage>>,
) -> Element<'a, PanelMessage> {
    let rows = summary.ranked();
    let rank = rows.iter().find(|r| r.run == run).map_or(0, |r| r.rank);
    let stats = summary
        .blocks
        .get(run)
        .map_or_else(LineStats::default, |b| b.stats);
    let trades: &[RoundTrip] = summary.trades.get(run).map_or(&[], Vec::as_slice);
    let profits: Vec<f64> = trades.iter().map(|t| t.profit).collect();
    let draws: Vec<usize> = (0..trades.len()).collect();
    let listed: Vec<&RoundTrip> = trades.iter().collect();
    trades_table(
        TradeTable {
            name: summary.blocks.get(run).map_or_else(String::new, block_name),
            stats,
            what: "This stretch of the market, backtested alone: its real trades, in order."
                .to_owned(),
            figures: run_figures((rank, rows.len()), &summarize_run(&draws, &profits)),
            capital,
            shown,
            top,
        },
        &listed,
    )
}

/// A block's dates: `Aug 16 – Aug 23 2026`.
pub(super) fn block_name(block: &BlockStats) -> String {
    let (from, to) = (
        DateTime::from_nanos(block.from),
        DateTime::from_nanos(block.to),
    );
    format!(
        "{} {:02} – {} {:02} {}",
        from.month_name(),
        from.day,
        to.month_name(),
        to.day,
        to.year
    )
}

/// `rows` drawn as a table, sorted as asked, with `picked` lit. The column naming them is `noun`,
/// with each row's name if there are any.
pub(super) fn runs_table<'a>(
    mut rows: Vec<RunRow>,
    sort: (RunColumn, bool),
    (noun, names): (&'static str, Vec<String>),
    capital: f64,
    ruin_at: f64,
    scroll: usize,
    picked: usize,
) -> Element<'a, PanelMessage> {
    sort_rows(&mut rows, sort);
    canvas(RunsTable {
        rows,
        noun,
        names,
        sort,
        capital,
        ruin_at,
        scroll,
        picked: Some(picked),
        top: 30.0,
    })
    .width(Fill)
    .height(Fill)
    .into()
}

/// The table of runs and the picked run's trades, half each, under the legend's first line.
pub(super) fn table_and_trades<'a>(
    table: Element<'a, PanelMessage>,
    trades: Element<'a, PanelMessage>,
) -> Element<'a, PanelMessage> {
    // From below the legend's first line, which runs across both.
    let divider = container(
        container(Space::new())
            .width(1)
            .height(Fill)
            .style(|_| container::background(theme::AXIS_LINE)),
    )
    .padding(Padding {
        top: 30.0,
        ..Padding::ZERO
    })
    .height(Fill);
    row![
        container(table)
            .padding(Padding {
                right: 6.0,
                left: 12.0,
                ..Padding::ZERO
            })
            .width(FillPortion(1))
            .height(Fill),
        divider,
        container(trades)
            .padding(Padding {
                top: 32.0,
                right: 8.0,
                bottom: 6.0,
                left: 8.0,
            })
            .width(FillPortion(1))
            .height(Fill),
    ]
    .into()
}

/// A run's figures, for the line over its trades.
pub(super) fn run_figures((rank, total): (usize, usize), summary: &RunSummary) -> String {
    if summary.trades == 0 {
        return format!("Rank {rank} of {total}. No trades.");
    }
    format!(
        "Rank {rank} of {total}  ·  {} of {} won  ·  best {}  ·  worst {}  ·  {} losses in a row",
        summary.winners,
        summary.trades,
        money(summary.best),
        money(summary.worst),
        summary.losing_streak,
    )
}

/// A Monte Carlo run's trades in the order it drew them.
pub(super) fn monte_carlo_trades<'a>(
    result: &MonteCarlo,
    run: usize,
    trades: &[RoundTrip],
    capital: f64,
    shown: Option<usize>,
    top: Option<Element<'a, PanelMessage>>,
) -> Element<'a, PanelMessage> {
    let backtest = run == result.all_stats.len();
    let line = result.line_of_run(run);
    let name = if backtest {
        "The backtest".to_owned()
    } else if line == Some(result.best) {
        "Best run".to_owned()
    } else if line == Some(result.worst) {
        "Worst run".to_owned()
    } else if line == Some(result.middle) {
        "Middle run".to_owned()
    } else {
        format!("Run {}", run + 1)
    };
    let draws = result.draws_of_run(run).unwrap_or_default();
    let profits: Vec<f64> = trades.iter().map(|t| t.profit).collect();
    let summary = summarize_run(&draws, &profits);
    let what = if backtest {
        "Every trade once, in the order it really came.".to_owned()
    } else {
        format!(
            "Drawn at random from the backtest's, so a trade can repeat or be left out: {} different ones of its {}.",
            summary.different, result.trades
        )
    };
    let rows = result.ranked_runs();
    let rank = rows.iter().find(|r| r.run == run).map_or(0, |r| r.rank);
    let listed: Vec<&RoundTrip> = draws.iter().filter_map(|&i| trades.get(i)).collect();
    trades_table(
        TradeTable {
            name,
            stats: result.stats_of_run(run).unwrap_or_default(),
            what,
            figures: run_figures((rank, rows.len()), &summary),
            capital,
            shown,
            top,
        },
        &listed,
    )
}

/// What goes over a table of trades.
pub(super) struct TradeTable<'a> {
    name: String,
    stats: LineStats,
    /// What these trades are, when the name's hovered.
    what: String,
    figures: String,
    capital: f64,
    /// The row last sent to the chart.
    shown: Option<usize>,
    /// Anything to go above it all.
    top: Option<Element<'a, PanelMessage>>,
}

/// Trades in order, each a click from where it really happened on the chart.
pub(super) fn trades_table<'a>(
    table: TradeTable<'a>,
    listed: &[&RoundTrip],
) -> Element<'a, PanelMessage> {
    let TradeTable {
        name,
        stats,
        what,
        figures,
        capital,
        shown,
        top,
    } = table;
    let cell = |content: String, color: Color, width: f32| {
        text(content)
            .size(HINT)
            .font(iced::Font::MONOSPACE)
            .color(color)
            .width(width)
    };
    let muted = |content: &str, width: f32| cell(content.to_owned(), theme::TEXT_MUTED, width);
    let color = if stats.return_ >= 0.0 {
        theme::UP
    } else {
        theme::DOWN
    };
    let header = row![
        tip(text(name).size(SMALL).color(theme::TEXT_BRIGHT), what, None),
        text(format!("{:+.2}%", stats.return_ * 100.0))
            .size(HINT)
            .font(iced::Font::MONOSPACE)
            .color(color),
        text(format!("fell {:.2}%", stats.drawdown * 100.0))
            .size(HINT)
            .font(iced::Font::MONOSPACE)
            .color(theme::TEXT),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let titles = row![
        muted("#", COLUMN_DRAW),
        muted("Opened", COLUMN_OPENED),
        muted("Side", COLUMN_SIDE),
        muted("Entry", COLUMN_PRICE),
        muted("Exit", COLUMN_PRICE),
        muted("Held", COLUMN_HELD),
        muted("Profit", COLUMN_PROFIT),
        muted("Equity", COLUMN_EQUITY),
    ]
    .spacing(4)
    .padding([0, 6]);
    let mut rows = column![].spacing(1);
    let mut equity = capital;
    for (place, trade) in listed.iter().enumerate() {
        equity += trade.profit;
        // A run can have thousands of trades; the figures still count them all.
        if place >= MAX_TRADE_ROWS {
            break;
        }
        let profit_color = if trade.profit >= 0.0 {
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
        let selected = shown == Some(place);
        let line = button(
            row![
                cell((place + 1).to_string(), theme::TEXT_MUTED, COLUMN_DRAW),
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
                cell(money(trade.profit), profit_color, COLUMN_PROFIT),
                cell(format!("{equity:.0}"), theme::TEXT, COLUMN_EQUITY),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([2, 6])
        .width(Fill)
        .on_press(PanelMessage::ShowTrade(place, trade.exit_bar))
        .style(move |_, status| result_style(selected, status));
        rows = rows.push(tip_at(
            line,
            format!(
                "{:+.2}% of the capital. Size {:.4}, fees {:.2}. Click to see where it really happened on the chart.",
                trade.profit / capital * 100.0,
                trade.qty,
                trade.fees,
            ),
            None,
            tooltip::Position::Left,
        ));
    }
    let mut list = column![].spacing(4).height(Fill);
    if let Some(top) = top {
        list = list.push(top);
    }
    let mut list = list.push(header);
    list = list.push(text(figures).size(HINT).color(theme::TEXT_MUTED));
    list = list.push(titles);
    list = list.push(
        scrollable(rows)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::hidden(),
            ))
            .height(Fill),
    );
    if listed.is_empty() {
        list = list.push(
            text("It made no trades in this stretch.")
                .size(HINT)
                .color(theme::TEXT_MUTED),
        );
    }
    if listed.len() > MAX_TRADE_ROWS {
        list = list.push(
            text(format!(
                "Showing the first {MAX_TRADE_ROWS} of {} trades.",
                listed.len()
            ))
            .size(HINT)
            .color(theme::TEXT_MUTED),
        );
    }
    list.into()
}

/// Every block, best first.
pub(super) fn blocks_list<'a>(
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
        rows = rows.push(line_row(
            block_name(block),
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
