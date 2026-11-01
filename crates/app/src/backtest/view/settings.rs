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
        tab(
            "Portfolio",
            TestKind::Portfolio,
            "Several markets on one account: they share cash and margin, and costs are the same as the backtest's. Hold them at weights, trade this chart's strategy on each, or follow a table of weights.",
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
    let stock_title = stock_title
        .push(tip(
            toggler(settings.no_costs)
                .label("No costs")
                .size(15)
                .text_size(HINT)
                .on_toggle(|on| PanelMessage::Edit(Edit::NoCosts(on)))
                .style(switch_style),
            "Nothing to pay at all: no fees, funding or cost settings. Stock off leaves a market's own fees and funding on; this doesn't.",
            None,
        ))
        .push(
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

    let entries_mode = |shifted: bool| -> Element<'a, PanelMessage> {
        tip(
            toggler(shifted)
                .label("Shifted")
                .size(14)
                .text_size(HINT)
                .on_toggle(PanelMessage::EntriesShifted)
                .style(switch_style),
            "Off places the trades at random, holding each as long. On keeps all the backtest's positions in order and moves them along the span, wrapping round, so only their timing goes.",
            None,
        )
    };
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
        TestKind::Portfolio => {
            let setup = &settings.portfolio;
            let run = match &panel.tests.portfolio {
                TestRun::Running { .. } => action(
                    "Cancel",
                    false,
                    Some(PanelMessage::StopPortfolio),
                    "Stop waiting for this portfolio.".to_owned(),
                ),
                TestRun::Done(_) => action(
                    "Run",
                    false,
                    can_run.then_some(PanelMessage::RunPortfolio),
                    run_or("Run it again, with whatever's changed. Orders go at the market, at the closes or next opens as the settings say."),
                ),
                _ => action(
                    "Run",
                    true,
                    can_run.then_some(PanelMessage::RunPortfolio),
                    run_or("Load these markets on this chart's timeframe and span, and run them on one account. Orders go at the market, at the closes or next opens as the settings say."),
                ),
            };
            let plain = LegChoice {
                label: if setup.use_chart_strategy {
                    "Chart's strategy".to_owned()
                } else {
                    "Holds its weight".to_owned()
                },
                choice: None,
            };
            let mut choices = vec![plain.clone()];
            choices.extend(info.strategies.iter().map(|choice| LegChoice {
                label: choice.name.clone(),
                choice: Some(choice.clone()),
            }));
            let hint = |content: &'a str, width: f32| {
                text(content).size(HINT).color(theme::TEXT_MUTED).width(width)
            };
            let mut legs = column![row![
                hint("Market", 92.0),
                hint("Venue", 56.0),
                hint("Weight", 50.0),
                hint("Trades", 130.0),
            ]
            .spacing(4)]
            .spacing(3);
            for (at, leg) in setup.legs.iter().enumerate() {
                let selected = match &leg.strategy {
                    None => plain.clone(),
                    Some(written) => {
                        let id = written.get("builtin").or_else(|| written.get("plugin"));
                        choices
                            .iter()
                            .find(|option| {
                                option.choice.as_ref().is_some_and(|choice| match &choice.kind {
                                    StrategyKind::BuiltIn(kind) => {
                                        id.and_then(serde_json::Value::as_str) == Some(kind.id())
                                    }
                                    StrategyKind::Plugin(plugin) => {
                                        id.and_then(serde_json::Value::as_str) == Some(plugin.as_str())
                                    }
                                })
                            })
                            .cloned()
                            .unwrap_or_else(|| LegChoice {
                                label: "Its own strategy".to_owned(),
                                choice: None,
                            })
                    }
                };
                legs = legs.push(
                    row![
                        tip(input(Field::LegSymbol(at), 92.0), "A market, as BTCUSDT, or BTCUSDT-PERP for a perpetual, which pays funding.", None),
                        tip(input(Field::LegVenue(at), 56.0), "Where its bars come from, for a market you imported. Blank is where this chart's come from.", None),
                        tip(input(Field::LegWeight(at), 50.0), "With no strategy, the signed share of the capital to hold: 0.5 is half, and -0.5 half of it short. With a strategy, what its position size is multiplied by, above 0. A table of weights takes over from this.", None),
                        tip(
                            pick_list(choices.clone(), Some(selected), move |choice| PanelMessage::LegStrategy(at, choice))
                                .text_size(HINT)
                                .padding([3, 6])
                                .width(130.0)
                                .style(|_, status| list_style(status))
                                .menu_style(|_| list_menu_style()),
                            "What decides this market's position. Each can trade its own, with its default values. A table of weights can't go with these.",
                            None,
                        ),
                        action("×", false, Some(PanelMessage::Edit(Edit::RemoveLeg(at))), "Take this market out.".to_owned()),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                );
            }
            let saved = tip(
                pick_list(
                    panel.tests.saved_portfolios.clone(),
                    None::<SavedPortfolio>,
                    PanelMessage::OpenPortfolio,
                )
                .on_open(PanelMessage::ListPortfolios)
                .placeholder("Saved runs")
                .text_size(HINT)
                .padding([3, 6])
                .width(110.0)
                .style(|_, status| list_style(status))
                .menu_style(|_| list_menu_style()),
                "Go back to a portfolio run from here or from Lab. Every one is kept, with Lab's runs.",
                None,
            );
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
            let table: Element<'a, PanelMessage> = match setup
                .weights
                .as_ref()
                .and_then(serde_json::Value::as_array)
            {
                Some(rows) => row![
                    text(format!("A table of {} rows", rows.len())).size(HINT).color(theme::TEXT_MUTED),
                    action("Clear", false, Some(PanelMessage::Edit(Edit::ClearWeights)), "Go back to the weights above.".to_owned()),
                ]
                .spacing(5)
                .align_y(Alignment::Center)
                .into(),
                None => action(
                    "Load weights",
                    false,
                    Some(PanelMessage::PickWeights),
                    "A JSON file of [[time, {\"BTCUSDT\": 0.5, \"ETHUSDT\": -0.5}], ...]. Each row states the whole book as signed shares of the account, from that bar's close.".to_owned(),
                ),
            };
            let chip_of = |label: &'a str, view: PortfolioView, what: &'a str| {
                tip(
                    button(text(label).size(HINT).center())
                        .padding([2, 7])
                        .on_press(PanelMessage::PortfolioView(view))
                        .style(move |_, status| chip(panel.tests.portfolio_view == view, status)),
                    what,
                    None,
                )
            };
            let view = panel.tests.portfolio_view;
            let chips = row![
                chip_of("Run", PortfolioView::Run, "One run of this book."),
                chip_of("Sweep", PortfolioView::Sweep, "Vary one thing from Min to Max by Step, into a table with the best first."),
                chip_of("Shifted", PortfolioView::Shifted, "The last run's book moved along the span by random amounts. If those do as well, its timing is worth nothing."),
                chip_of("Monte Carlo", PortfolioView::MonteCarlo, "The last run's returns drawn again in blocks, keeping how the markets moved together."),
                chip_of("Walk-forward", PortfolioView::WalkForward, "Each window picks the best of the sweep's values on its stretch, and trades it on the unseen one after."),
            ]
            .spacing(3);
            let mut vary_choices = vec![
                VaryChoice { label: Vary::Scale.name(setup), vary: Vary::Scale },
                VaryChoice { label: Vary::RebalanceEvery.name(setup), vary: Vary::RebalanceEvery },
                VaryChoice { label: Vary::RebalanceDrift.name(setup), vary: Vary::RebalanceDrift },
                VaryChoice { label: Vary::SlippagePercent.name(setup), vary: Vary::SlippagePercent },
                VaryChoice { label: Vary::Leverage.name(setup), vary: Vary::Leverage },
            ];
            for at in 0..setup.legs.len() {
                let vary = Vary::Weight(at);
                vary_choices.push(VaryChoice { label: vary.name(setup), vary });
            }
            let vary_selected = VaryChoice {
                label: panel.tests.portfolio_vary.name(setup),
                vary: panel.tests.portfolio_vary,
            };
            let test_run = |busy: bool, done: bool, what: &str| {
                if busy {
                    action("Cancel", false, Some(PanelMessage::StopPortfolioTest), "Stop it; what it has so far is dropped.".to_owned())
                } else {
                    action("Run", !done, can_run.then_some(PanelMessage::RunPortfolio), run_or(what))
                }
            };
            let busy_or_done = |busy: bool, done: bool| (busy, done);
            let sweep_row = |busy_done: (bool, bool), what: &'static str| -> Element<'a, PanelMessage> {
                row![
                    pick_list(vary_choices.clone(), Some(vary_selected.clone()), PanelMessage::PortfolioVary)
                        .text_size(HINT)
                        .padding([3, 6])
                        .width(130.0)
                        .style(|_, status| list_style(status))
                        .menu_style(|_| list_menu_style()),
                    small("Min", Field::SweepMin, "Where the sweep starts."),
                    small("Max", Field::SweepMax, "Where it ends."),
                    small("Step", Field::SweepStep, "How far between values."),
                    container(test_run(busy_done.0, busy_done.1, what)).align_right(Fill),
                ]
                .spacing(5)
                .align_y(Alignment::Center)
                .into()
            };
            let running = |run: bool, done: bool| busy_or_done(run, done);
            let extra: Option<Element<'a, PanelMessage>> = match view {
                PortfolioView::Run => None,
                PortfolioView::Sweep => Some(sweep_row(
                    running(matches!(panel.tests.portfolio_sweep, TestRun::Running { .. }), matches!(panel.tests.portfolio_sweep, TestRun::Done(_))),
                    "Run every value. With a table of weights, weights can't be varied.",
                )),
                PortfolioView::WalkForward => Some(
                    column![
                        sweep_row(
                            running(matches!(panel.tests.portfolio_walk, TestRun::Running { .. }), matches!(panel.tests.portfolio_walk, TestRun::Done(_))),
                            "Pick among these values on each stretch, and trade the pick on the unseen one after.",
                        ),
                        row![
                            small("Windows", Field::Windows, "How many times to slide along."),
                            small("Ratio", Field::WalkRatio, "How many times longer than its unseen stretch each window picks on."),
                        ]
                        .spacing(6),
                    ]
                    .spacing(4)
                    .into(),
                ),
                PortfolioView::Shifted => Some(
                    row![
                        small("Runs", Field::Entries, "How many times to move it along."),
                        container(test_run(
                            matches!(panel.tests.portfolio_shifted, TestRun::Running { .. }),
                            matches!(panel.tests.portfolio_shifted, TestRun::Done(_)),
                            "Move the last run's book along the span, that many times.",
                        ))
                        .align_right(Fill),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .into(),
                ),
                PortfolioView::MonteCarlo => Some(
                    row![
                        small("Runs", Field::Runs, "How many times to draw."),
                        small("Ruin %", Field::Ruin, "The fall counted as ruin."),
                        small("Block", Field::PortfolioBlock, "How many bars a block is. Longer keeps more of how returns follow each other."),
                        container(test_run(
                            false,
                            matches!(panel.tests.portfolio_monte_carlo, TestRun::Done(_)),
                            "Draw the last run's returns again in blocks.",
                        ))
                        .align_right(Fill),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .into(),
                ),
            };
            let mut below = column![chips].spacing(5);
            if let Some(extra) = extra {
                below = below.push(extra);
            }
            Some(
                column![
                    below,
                    legs,
                    row![
                        saved,
                        action("Add market", false, Some(PanelMessage::Edit(Edit::AddLeg)), "Add a market to trade on the same account.".to_owned()),
                        table,
                        tip(
                            toggler(setup.use_chart_strategy)
                                .label("Chart's")
                                .size(14)
                                .text_size(HINT)
                                .on_toggle(|on| PanelMessage::Edit(Edit::LegsUseStrategy(on))),
                            "Every market trades this chart's strategy at its weight, rather than holding its weight.",
                            None,
                        ),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                    row![
                        small("Every", Field::RebalanceEvery, "Go back to the weights every so many bars. Blank never does. Rebalancing sizes from the account as it is now."),
                        small("Drift", Field::RebalanceDrift, "Go back to the weights when a leg is this many points of the account away. Blank never does."),
                        small("Min", Field::RebalanceMinTrade, "Trades worth less than this aren't made, so small changes don't pay fees for nothing."),
                        small("Margin %", Field::Maintenance, "What a position needs to stay open, as a percentage of its value. Everything is liquidated at once when the account falls to it, whatever the volume limit."),
                        container(if view == PortfolioView::Run { run } else { Space::new().into() }).align_right(Fill),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                ]
                .spacing(5)
                .into(),
            )
        }
        TestKind::RandomEntries if panel.tests.entries_shifted => {
            let run = match &panel.tests.shifted {
                TestRun::Running { .. } => action(
                    "Cancel",
                    false,
                    Some(PanelMessage::StopShifted),
                    "Stop these runs; what they've found so far is dropped.".to_owned(),
                ),
                TestRun::Done(_) => action(
                    "Run again",
                    false,
                    can_run.then_some(PanelMessage::RunShifted),
                    run_or("Move it by other amounts. Otherwise the same inputs give the same results."),
                ),
                _ => action(
                    "Run",
                    true,
                    can_run.then_some(PanelMessage::RunShifted),
                    run_or("Backtest the same positions moved along the span, that many times over."),
                ),
            };
            Some(
                row![
                    entries_mode(true),
                    boxed(
                        "Runs",
                        Field::Entries,
                        "",
                        "How many times to move it along. Press Enter to apply.",
                    ),
                    container(run).align_right(Fill),
                ]
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
                    entries_mode(false),
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
                row![
                    small(
                        "Windows",
                        Field::Windows,
                        "How many times to slide along.",
                    ),
                    small(
                        "Ratio",
                        Field::WalkRatio,
                        "How many times longer than its unseen stretch each window picks on.",
                    ),
                ]
                .spacing(6)
                .into()
            } else {
                row![
                    keep,
                    small(
                        "Pick on %",
                        Field::HoldOutShare,
                        "With Keep unseen on, how much of the span values are picked on. The rest stays unseen.",
                    ),
                ]
                .spacing(6)
                .align_y(Alignment::Center)
                .into()
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
    const PARTICIPATION: &str = "The most of a bar's volume an order can take. What's left goes out on the next bars at their prices, so big positions take time to build and to leave. A resting limit fills no more than this of each bar.";
    const RANGE_SLIPPAGE: &str = "What a market order or stop pays on top, as a percentage of a bar's high to low, times how many times wider that bar is than the median of the last 100. A wild bar costs far more than a quiet one. A rough assumption.";
    const WARNINGS: &str = "Warn when an order's over this share of its bar's volume, with Impact on, or when fewer than this share of limit orders fill.";
    const WARM_UP: &str = "How many bars before the span strategies warm up on. Raise it for look-backs longer than this. Press Enter to load them.";
    const CASH_INTEREST: &str = "What cash below zero costs in a portfolio, as a percentage a year. Without it, a book holding more than the account borrows free.";
    const TRAILING: &str = "A real stop this far from the best price since entry, moved at each close, so it only tightens. With Stop loss too, whichever is nearer goes first.";
    const DECISIONS: &str = "Keep a row for each bar: what the strategy asked for, what was held, what went out, and why not. Shows as Decisions over the results.";
    const JITTER: &str = "How much longer than the latency orders take on average, drawn for each order. Most are quick and a few slow. Only fills on ticks can show it.";
    const SEED: &str = "Which draws of jitter a run uses, so it can be repeated.";
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
    // These have more to them, so get the width of the section.
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
        line(
            switch(settings.range_slippage_percent.on, Edit::RangeSlippage, RANGE_SLIPPAGE),
            name("Range slip", RANGE_SLIPPAGE),
            tip(
                row![input(Field::RangeSlippage, 54.0), unit("% over"), input(Field::RangeLookBack, 46.0), unit("bars")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                RANGE_SLIPPAGE,
                None,
            ),
        ),
        line(
            switch(settings.impact_percent.on, Edit::Impact, IMPACT),
            name("Impact", IMPACT),
            tip(
                row![input(Field::Impact, 54.0), unit("% power"), input(Field::ImpactExponent, 40.0)]
                    .spacing(3)
                    .align_y(Alignment::Center),
                IMPACT,
                None,
            ),
        ),
        line(
            no_switch(),
            name("Warn at", WARNINGS),
            tip(
                row![input(Field::VolumeWarning, 46.0), unit("% volume"), input(Field::FillWarning, 46.0), unit("% filled")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                WARNINGS,
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
            switch(settings.trailing_stop_percent.on, Edit::TrailingStop, TRAILING),
            name("Trailing stop", TRAILING),
            tip(
                row![input(Field::TrailingStop, 54.0), unit("%")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                TRAILING,
                None,
            ),
        ),
        line(
            switch(settings.decision_log, Edit::DecisionLog, DECISIONS),
            name("Decision log", DECISIONS),
            Space::new().into(),
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
            switch(settings.max_participation_percent.on, Edit::Participation, PARTICIPATION),
            name("Volume limit", PARTICIPATION),
            tip(
                row![input(Field::Participation, 54.0), unit("%")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                PARTICIPATION,
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
            switch(settings.cash_interest_percent_per_year.on, Edit::CashInterest, CASH_INTEREST),
            name("Cash interest", CASH_INTEREST),
            tip(
                row![input(Field::CashInterest, 54.0), unit("%")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                CASH_INTEREST,
                None,
            ),
        ),
        line(
            no_switch(),
            name("Warm-up", WARM_UP),
            tip(
                row![input(Field::WarmUp, 54.0), unit("bars")]
                    .spacing(3)
                    .align_y(Alignment::Center),
                WARM_UP,
                None,
            ),
        ),
        line(
            switch(settings.latency_jitter.on, Edit::LatencyJitter, JITTER),
            name("Jitter", JITTER),
            ticks_box(Field::LatencyJitter, "ms", JITTER),
        ),
        line(
            no_switch(),
            name("Jitter seed", SEED),
            tip(input(Field::LatencySeed, 92.0), SEED, None),
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
