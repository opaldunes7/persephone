//! The panel's left side: the strategy, the test and every setting.

use iced::widget::column;

use super::*;

/// The panel's left side: the strategy, every stock setting and the plugin slots, packed into
/// as many columns as fit. What each one does is in the tooltips of its name and controls.
pub(super) fn settings_section<'a>(info: &PanelInfo<'a>) -> Element<'a, PanelMessage> {
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
                "{} of 8 on, capital {}",
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
    let typed = panel
        .inputs
        .iter()
        .any(|(f, _)| f.belongs_to(settings.test));
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
    const INVERT: &str = "Trade the opposite of what the strategy says: sell when it buys and buy when it sells, at the same moments. Paper trading follows it too.";
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
        line(
            switch(settings.invert, Edit::Invert, INVERT),
            name("Invert", INVERT),
            no_switch(),
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
