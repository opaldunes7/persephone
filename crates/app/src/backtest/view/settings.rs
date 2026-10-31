//! The panel's left side: the strategy, the test and every setting.

use iced::widget::column;

use super::*;

/// The panel's left side: the strategy, every stock setting and the plugin slots, packed into
/// as many columns as fit. What each one does is in the tooltips of its name and controls.
/// Which test to run, all on one line across the top of the panel.
pub(super) fn test_tabs<'a>(current: TestKind, panel: &'a Panel) -> Element<'a, PanelMessage> {
    let tab = |label: &'a str, test: TestKind, what: &'a str| {
        tip(
            button(text(label).size(SMALL).center())
                .padding([3, 8])
                .on_press(PanelMessage::Test(test))
                .style(move |_, status| chip(current == test, status)),
            what,
            None,
        )
    };
    let tabs = row![
        tip(
            text("Test").size(SMALL).color(theme::TEXT_BRIGHT),
            "How to test the strategy. One backtest can't tell skill from luck; the others show how far its result could have gone either way, and whether it beats chance.",
            None,
        ),
        tab(
            "Backtest",
            TestKind::Backtest,
            "One run through the span, which can be played back bar by bar.",
        ),
        tab(
            "Monte Carlo",
            TestKind::MonteCarlo,
            "Draws the backtest's trades at random many times, showing the range of results and drawdowns those trades could have given.",
        ),
        tab(
            "Random blocks",
            TestKind::RandomBlocks,
            "Backtests random stretches of the span, showing whether the strategy works throughout or only in one period.",
        ),
        tab(
            "Optimize",
            TestKind::Optimize,
            "Tries a range of one or two of the strategy's variables and shows which values do best and how steady they are.",
        ),
        tab(
            "Walk-forward",
            TestKind::WalkForward,
            "Optimizes on a stretch, tries the best values on the unseen stretch after it, and slides along. The fairest test of whether optimizing helps.",
        ),
        tab(
            "Random entries",
            TestKind::RandomEntries,
            "Places the backtest's trades at random times many times over, keeping how long each was held. If that does as well, the entries are worth nothing.",
        ),
    ]
    .spacing(3)
    .align_y(Alignment::Center);
    let action = |label: &'a str, message: PanelMessage, what: &'a str| {
        tip(
            button(text(label).size(SMALL).center())
                .padding([3, 8])
                .on_press(message)
                .style(|_, status| chip(false, status)),
            what,
            None,
        )
    };
    let notice = panel.notice.as_ref().map(|(said, failed)| {
        text(said.as_str()).size(HINT).color(if *failed {
            theme::DOWN
        } else {
            theme::TEXT_MUTED
        })
    });
    row![
        tabs,
        container(notice).width(Fill).padding([0, 12]),
        action(
            "Export chart",
            PanelMessage::ExportChart,
            "Save this chart's settings, strategy and values to a JSON file.",
        ),
        action(
            "Export all",
            PanelMessage::ExportAll,
            "Save your saved results, layout, drawings and which plugins are in use to one JSON file. Never API keys, plugins' code or market data.",
        ),
        tip(
            row![
                toggler(panel.export_runs)
                    .size(15)
                    .on_toggle(PanelMessage::ExportRuns)
                    .style(switch_style),
                text("Runs").size(SMALL).color(theme::TEXT),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
            "Include every saved run in full when exporting all. They can be large.",
            None,
        ),
        action(
            "Import",
            PanelMessage::Import,
            "Read an exported file. Results are added beside yours, and it asks before replacing your layout.",
        ),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}

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

    // Which test is in the bar over the panel; these are its own settings.
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
        TestKind::RandomEntries => {
            let run = match &panel.tests.random_entries {
                TestRun::Running { .. } => action(
                    "Cancel",
                    false,
                    Some(PanelMessage::StopRandomEntries),
                    "Stop these runs; what they've found so far is dropped.".to_owned(),
                ),
                TestRun::Done(_) => action(
                    "Run again",
                    false,
                    can_run.then_some(PanelMessage::RunAgain),
                    run_or("Place them at other random times. Otherwise the same inputs give the same results."),
                ),
                _ => action(
                    "Run",
                    true,
                    can_run.then_some(PanelMessage::RunRandomEntries),
                    run_or("Backtest the same holds at random times, that many times over."),
                ),
            };
            Some(
                row![
                    boxed(
                        "Runs",
                        Field::Entries,
                        "",
                        "How many times to place the trades at random. Press Enter to apply.",
                    ),
                    container(run).align_right(Fill),
                ]
                .spacing(10)
                .align_y(Alignment::Center)
                .into(),
            )
        }
        TestKind::Optimize | TestKind::WalkForward => {
            let walking = settings.test == TestKind::WalkForward;
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
                pick_list(
                    if walking {
                        &ScoreBy::ALL[..2]
                    } else {
                        &ScoreBy::ALL[..]
                    },
                    Some(chosen.score),
                    PanelMessage::ScoreBy,
                )
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
            let run = match &panel.tests.walk_forward {
                TestRun::Running { .. } if walking => action(
                    "Cancel",
                    false,
                    Some(PanelMessage::StopWalkForward),
                    "Stop this walk-forward; what it has found so far is dropped.".to_owned(),
                ),
                TestRun::Done(_) if walking => action(
                    "Run again",
                    false,
                    can_run.then_some(PanelMessage::RunAgain),
                    run_or("Run it again with new random numbers."),
                ),
                _ if walking => action(
                    "Run",
                    true,
                    can_run.then_some(PanelMessage::RunWalkForward),
                    run_or("Optimize and test each window in turn. Orders fill on the chart's bars."),
                ),
                _ => match &panel.tests.optimize {
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
                },
            };
            let extra: Element<'a, PanelMessage> = if walking {
                small(
                    "Windows",
                    Field::Windows,
                    "How many times to slide along: each picks on a stretch three times the length of the unseen one after it.",
                )
            } else {
                keep
            };
            let mut rows = column![
                variable_row(0),
                variable_row(1),
                row![score, extra, container(run).align_right(Fill)]
                    .spacing(8)
                    .align_y(Alignment::Center),
            ]
            .spacing(5);
            if chosen.score == ScoreBy::RandomBlocks && !walking {
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
    let rates = matches!(settings.commission.value, Commission::Rates { .. });
    let fees_taker: Element<'a, PanelMessage> = if rates {
        tip(
            input(Field::Taker, 46.0),
            "A taker's fee, as a percentage.",
            None,
        )
    } else {
        Space::new().width(0).into()
    };
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
    const SLIPPAGE_PERCENT: &str = "Every market order fills this percentage of the price worse, which means the same on any market.";
    const NEXT_OPEN: &str = "Fill orders at the next bar's open instead of the close they were decided on: a realistic delay, with no finer data needed. Not with Latency or High precision.";
    const LIMIT: &str = "Send orders as limits this far better than the close, resting this many bars. They only fill when price trades through them, at their own price. An entry that doesn't fill lapses, and an exit goes at the market.";
    const STOP: &str = "A real stop this far from the entry, from the bar after it. It fills inside a bar at its level, or at the open on a gap.";
    const TARGET: &str = "A real target this far from the entry, from the bar after it. On a bar reaching both, the stop's assumed to have come first.";
    const IMPACT: &str = "How far a market order fills worse when it's all of its bar's volume. It moves with the square root of its share of that volume. A rough assumption.";
    const BORROW: &str = "What borrowing costs a short, as a percentage of its value a year. Perpetuals pay funding instead.";
    const COMPOUND: &str = "Size positions from the account as it is now, so profits grow them and losses shrink them. Off, every position is sized from the starting capital.";
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
    // These two have more to them, so get the width of the section.
    let wide: Vec<Element<'a, PanelMessage>> = vec![
        line(
            switch(settings.commission.on, Edit::Commission, FEES),
            name("Fees", FEES),
            row![
                choice(
                    "%",
                    !per_order && !rates,
                    Edit::PerOrder(false),
                    "A percentage of each fill's value.",
                ),
                choice(
                    "flat",
                    per_order,
                    Edit::PerOrder(true),
                    "The same amount for every order, in the quote currency.",
                ),
                choice(
                    "m/t",
                    rates,
                    Edit::MakerTaker(!rates),
                    "Different rates for orders that rest and those that take. A maker's can be below zero, for a rebate.",
                ),
                tip(input(Field::Commission, 54.0), FEES, None),
                fees_taker,
            ]
            .spacing(2)
            .align_y(Alignment::Center)
            .into(),
        ),
        line(
            switch(settings.limit.on, Edit::Limit, LIMIT),
            name("Limit orders", LIMIT),
            tip(
                row![
                    input(Field::LimitOffset, 54.0),
                    unit("%"),
                    input(Field::LimitBars, 46.0),
                    unit("bars"),
                    tip(
                        row![unit("through"), input(Field::LimitThrough, 46.0), unit("%")]
                            .spacing(3)
                            .align_y(Alignment::Center),
                        "How far past its price the market must trade before it fills, standing in for the queue ahead of it, which a bar can't show. It still pays its own price. Raise it to see how much a result leans on fills that might not have happened.",
                        None,
                    ),
                ]
                .spacing(3)
                .align_y(Alignment::Center),
                LIMIT,
                None,
            ),
        ),
    ];
    let simulation = vec![
        line(
            no_switch(),
            name("Capital", CAPITAL),
            tip(input(Field::Capital, 92.0), CAPITAL, None),
        ),
        line(
            switch(settings.slippage.on, Edit::Slippage, SLIPPAGE),
            name("Slippage", SLIPPAGE),
            ticks_box(Field::Slippage, "ticks", SLIPPAGE),
        ),
        line(
            switch(settings.slippage_percent.on, Edit::SlippagePercent, SLIPPAGE_PERCENT),
            name("Slippage %", SLIPPAGE_PERCENT),
            tip(
                row![input(Field::SlippagePercent, 54.0), unit("%")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                SLIPPAGE_PERCENT,
                None,
            ),
        ),
        line(
            switch(settings.stop_loss_percent.on, Edit::StopLoss, STOP),
            name("Stop loss", STOP),
            tip(
                row![input(Field::StopLoss, 54.0), unit("%")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                STOP,
                None,
            ),
        ),
        line(
            switch(settings.take_profit_percent.on, Edit::TakeProfit, TARGET),
            name("Target", TARGET),
            tip(
                row![input(Field::TakeProfit, 54.0), unit("%")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                TARGET,
                None,
            ),
        ),
        line(
            switch(settings.impact_percent.on, Edit::Impact, IMPACT),
            name("Impact", IMPACT),
            tip(
                row![input(Field::Impact, 54.0), unit("%")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                IMPACT,
                None,
            ),
        ),
        line(
            switch(settings.borrow_percent_per_year.on, Edit::Borrow, BORROW),
            name("Borrow a year", BORROW),
            tip(
                row![input(Field::Borrow, 54.0), unit("%")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                BORROW,
                None,
            ),
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
        line(
            switch(settings.compound, Edit::Compound, COMPOUND),
            name("Compound", COMPOUND),
            no_switch(),
        ),
        line(
            switch(
                settings.fill_timing == FillTiming::NextOpen,
                |on| {
                    Edit::FillTiming(if on {
                        FillTiming::NextOpen
                    } else {
                        FillTiming::Close
                    })
                },
                NEXT_OPEN,
            ),
            name("Fill at next open", NEXT_OPEN),
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
    let mut body = column![top].spacing(7);
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
        body = body
            .extend(wide)
            .push(grid(simulation, columns))
            .push(session_row);
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
