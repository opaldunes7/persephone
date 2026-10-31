//! Strategies on charts, driven through the same messages the UI sends. Backtests run on the
//! test thread here (the app would hand them to a worker) and come back as `BacktestDone`.

use std::{path::PathBuf, sync::Arc};

use iced::{Point, Size};
use persephone::{
    Message, Persephone, Run,
    backtest::{Edit, Field, PanelMessage, StrategyChoice},
    keymap::Action,
    menu::{MenuMessage, Page, Popup},
};
use persephone_chart::{ChartData, ChartMessage};
use persephone_core::{
    Candle, Timeframe,
    indicator::BuiltIn,
    strategy::{BuiltInStrategy, StrategyKind, StrategySpec},
};

fn app() -> Persephone {
    let candles = (0..1_500u64)
        .map(|i| {
            let p = 50_000.0 + 1_500.0 * (i as f64 / 45.0).sin();
            Candle {
                ts: i * Timeframe::H1.nanos(),
                open: p - 5.0,
                high: p + 25.0,
                low: p - 25.0,
                close: p + 5.0,
                volume: 3.0,
            }
        })
        .collect();
    Persephone::preloaded(
        ChartData::new("BTCUSDT", "BINANCE", Timeframe::H1, 2, candles),
        Size::new(1400.0, 900.0),
    )
}

fn menu(app: &mut Persephone, message: MenuMessage) {
    let _ = app.update(Message::Menu(message));
}

fn set_strategy(app: &mut Persephone, kind: BuiltInStrategy) {
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::RightClicked {
            at: Point::new(200.0, 200.0),
        },
    ));
    menu(app, MenuMessage::Open(Page::Strategies));
    menu(app, MenuMessage::SetStrategy(kind));
}

/// Runs the backtest the app asked for and delivers the result.
fn finish_backtest(app: &mut Persephone) {
    let (generation, request) = app.backtest_request(0).expect("a backtest to run");
    let result = persephone_engine::run_backtest(&request)
        .map(Arc::new)
        .map_err(|e| e.to_string());
    let _ = app.update(Message::BacktestDone(0, generation, result));
}

fn run_state(app: &Persephone) -> &Run {
    app.tile_strategy(0).expect("a strategy").1
}

#[test]
fn a_strategy_backtests_and_draws_its_trades_and_equity() {
    let mut app = app();
    set_strategy(&mut app, BuiltInStrategy::EmaCross);
    assert!(app.popup().is_none());
    assert!(matches!(run_state(&app), Run::Running));

    finish_backtest(&mut app);
    let Run::Done(report) = run_state(&app) else {
        panic!("{:?}", run_state(&app))
    };
    assert!(!report.fills.is_empty());

    let chart = app.tile_chart(0).unwrap();
    let trades = chart.trades().expect("trades on the chart");
    assert_eq!(trades.markers.len(), report.fills.len());
    assert!(!trades.spans.is_empty());
    assert_eq!(chart.studies().last().unwrap().label, "Equity");
}

#[test]
fn indicators_and_the_equity_pane_coexist() {
    let mut app = app();
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::RightClicked {
            at: Point::new(5.0, 5.0),
        },
    ));
    menu(&mut app, MenuMessage::AddIndicator(BuiltIn::Rsi));
    set_strategy(&mut app, BuiltInStrategy::DonchianBreakout);
    finish_backtest(&mut app);

    let labels: Vec<_> = app
        .tile_chart(0)
        .unwrap()
        .studies()
        .iter()
        .map(|s| s.label.clone())
        .collect();
    assert_eq!(labels, ["RSI 14", "Equity"]);

    // The equity legend sits after the indicators; clicking it opens the report.
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::StudyClicked {
            study: 1,
            at: Point::new(20.0, 500.0),
        },
    ));
    assert!(matches!(app.popup(), Some(Popup::Report { .. })));
}

#[test]
fn results_from_superseded_runs_are_dropped() {
    let mut app = app();
    set_strategy(&mut app, BuiltInStrategy::EmaCross);
    let (stale, request) = app.backtest_request(0).unwrap();

    let _ = app.update(Message::OpenReport(0));
    menu(&mut app, MenuMessage::StrategyParamChanged(0, "5".into()));
    let (current, _) = app.backtest_request(0).unwrap();
    assert!(current > stale);

    let old = persephone_engine::run_backtest(&request)
        .map(Arc::new)
        .map_err(|e| e.to_string());
    let _ = app.update(Message::BacktestDone(0, stale, old));
    assert!(
        matches!(run_state(&app), Run::Running),
        "the old result is ignored"
    );

    finish_backtest(&mut app);
    assert!(matches!(run_state(&app), Run::Done(_)));
    assert_eq!(app.tile_strategy(0).unwrap().0.params[0], 5.0);
}

#[test]
fn report_edits_are_validated() {
    let mut app = app();
    set_strategy(&mut app, BuiltInStrategy::EmaCross);
    let _ = app.update(Message::OpenReport(0));
    let (before, _) = app.backtest_request(0).unwrap();

    menu(
        &mut app,
        MenuMessage::StrategyParamChanged(0, "fast".into()),
    );
    assert!(matches!(
        app.popup(),
        Some(Popup::Report { error: Some(_), .. })
    ));
    assert_eq!(app.backtest_request(0).unwrap().0, before, "nothing reran");

    // In range but slower than Slow: accepted as typed, and the engine explains the problem.
    menu(&mut app, MenuMessage::StrategyParamChanged(0, "50".into()));
    finish_backtest(&mut app);
    let Run::Failed(message) = run_state(&app) else {
        panic!("{:?}", run_state(&app))
    };
    assert!(message.contains("Fast"), "{message}");
    assert!(
        app.tile_chart(0).unwrap().trades().is_none(),
        "no stale trades"
    );
}

#[test]
fn removing_the_strategy_clears_the_chart() {
    let mut app = app();
    set_strategy(&mut app, BuiltInStrategy::RsiReversion);
    finish_backtest(&mut app);
    let _ = app.update(Message::OpenReport(0));
    menu(&mut app, MenuMessage::RemoveStrategy);

    assert!(app.tile_strategy(0).is_none());
    let chart = app.tile_chart(0).unwrap();
    assert!(chart.trades().is_none());
    assert!(chart.studies().is_empty());
}

#[test]
fn switching_timeframe_keeps_the_strategy_and_waits_for_bars() {
    let mut app = app();
    set_strategy(&mut app, BuiltInStrategy::EmaCross);
    finish_backtest(&mut app);

    let _ = app.update(Message::Chart(
        0,
        ChartMessage::RightClicked {
            at: Point::new(5.0, 5.0),
        },
    ));
    menu(&mut app, MenuMessage::SetTimeframe(Timeframe::H4));
    assert_eq!(
        app.tile_strategy(0).unwrap().0,
        &StrategySpec::new(BuiltInStrategy::EmaCross)
    );
    assert!(matches!(run_state(&app), Run::Waiting));
    assert!(app.backtest_request(0).is_none(), "no bars yet");
}

#[test]
fn strategies_are_saved_and_restored() {
    let dir = std::env::temp_dir().join(format!("persephone-strategy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path: PathBuf = dir.join("layout.ron");

    let (mut first, _loads) = Persephone::start(&[], Some(path.clone()));
    let _ = first.update(Message::Chart(
        0,
        ChartMessage::RightClicked {
            at: Point::new(10.0, 10.0),
        },
    ));
    let _ = first.update(Message::Menu(MenuMessage::SetStrategy(
        BuiltInStrategy::DonchianBreakout,
    )));

    let (restored, _loads) = Persephone::start(&[], Some(path));
    let (spec, run) = restored.tile_strategy(0).expect("restored");
    assert_eq!(spec, &StrategySpec::new(BuiltInStrategy::DonchianBreakout));
    assert!(matches!(run, Run::Waiting), "reruns once bars arrive");
    std::fs::remove_dir_all(dir).unwrap();
}

mod sweeps {
    use iced::Point;
    use persephone::{
        Message,
        keymap::Action,
        menu::{MenuMessage, Popup},
    };
    use persephone_chart::ChartMessage;
    use persephone_core::strategy::BuiltInStrategy;

    fn with_strategy() -> persephone::Persephone {
        let mut app = super::app();
        let _ = app.update(Message::Chart(
            0,
            ChartMessage::RightClicked {
                at: Point::new(9.0, 9.0),
            },
        ));
        let _ = app.update(Message::Menu(MenuMessage::SetStrategy(
            BuiltInStrategy::EmaCross,
        )));
        let _ = app.update(Message::OpenReport(0));
        let _ = app.update(Message::Menu(MenuMessage::OpenSweep));
        app
    }

    #[test]
    fn a_sweep_opens_a_chart_per_value_on_a_fresh_workspace() {
        let mut app = with_strategy();
        let Some(Popup::Sweep { param, inputs, .. }) = app.popup() else {
            panic!("no sweep popup")
        };
        assert_eq!(*param, 0);
        assert_eq!(
            inputs,
            &["5".to_owned(), "20".to_owned(), "4".to_owned()],
            "Fast 10: half to double"
        );

        let _ = app.update(Message::Menu(MenuMessage::StartSweep));
        assert!(app.popup().is_none());
        assert_eq!(app.workspace(), 2);
        let tiles = app.layout().tiles();
        let fast: Vec<f64> = tiles
            .iter()
            .map(|&id| app.tile_strategy(id).unwrap().0.params[0])
            .collect();
        assert_eq!(fast, vec![5.0, 10.0, 15.0, 20.0]);
        assert!(
            tiles
                .iter()
                .all(|&id| app.tile_market(id) == app.tile_market(tiles[0]))
        );
        assert_eq!(
            app.workspaces(),
            vec![1, 2],
            "the original stays on workspace 1"
        );
    }

    #[test]
    fn a_sweep_explains_bad_ranges_and_full_workspaces() {
        let mut app = with_strategy();
        let _ = app.update(Message::Menu(MenuMessage::SweepInput(2, "40".into())));
        let _ = app.update(Message::Menu(MenuMessage::StartSweep));
        let Some(Popup::Sweep { error, .. }) = app.popup() else {
            panic!()
        };
        assert!(
            error.as_deref().is_some_and(|e| e.contains("2 to 9")),
            "{error:?}"
        );

        let _ = app.update(Message::Menu(MenuMessage::SweepParam(1)));
        let Some(Popup::Sweep { inputs, .. }) = app.popup() else {
            panic!()
        };
        assert_eq!(inputs[0], "15", "Slow 30: from 15");

        let mut full = super::app();
        for n in 2..=9 {
            let _ = full.update(Message::Key(Action::Workspace(n)));
        }
        let _ = full.update(Message::Key(Action::Workspace(1)));
        let _ = full.update(Message::Chart(
            0,
            ChartMessage::RightClicked {
                at: Point::new(9.0, 9.0),
            },
        ));
        let _ = full.update(Message::Menu(MenuMessage::SetStrategy(
            BuiltInStrategy::EmaCross,
        )));
        let _ = full.update(Message::OpenReport(0));
        let _ = full.update(Message::Menu(MenuMessage::OpenSweep));
        let _ = full.update(Message::Menu(MenuMessage::StartSweep));
        let Some(Popup::Sweep { error, .. }) = full.popup() else {
            panic!()
        };
        assert!(
            error
                .as_deref()
                .is_some_and(|e| e.contains("every workspace")),
            "{error:?}"
        );
    }
}

mod live {
    use std::time::Duration;

    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::{Action, task::into_stream};
    use persephone::live::LiveState;
    use persephone_engine::live::LiveMode;

    use super::*;

    /// Bars after the app's 1,500, swinging fast enough for EMA crosses.
    fn more_bars(n: u64) -> Vec<Candle> {
        (1_500..1_500 + n)
            .map(|i| {
                let p = 50_000.0 + 2_500.0 * ((i - 1_500) as f64 / 6.0).sin();
                Candle {
                    ts: i * Timeframe::H1.nanos(),
                    open: p - 5.0,
                    high: p + 25.0,
                    low: p - 25.0,
                    close: p + 5.0,
                    volume: 3.0,
                }
            })
            .collect()
    }

    #[test]
    fn paper_trading_from_the_report_follows_live_bars() {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        finish_backtest(&mut app);
        let replay = more_bars(80);
        app.rehearse_live(replay.clone(), Duration::from_millis(3));

        let _ = app.update(Message::OpenReport(0));
        let task = app.update(Message::Menu(MenuMessage::GoLive(LiveMode::Paper)));
        assert_eq!(app.tile_live(0).unwrap().state, LiveState::Connecting);
        let mut events = into_stream(task).expect("the session's events");

        // Deliver the session's events as the app would; stop once every bar is in. (The
        // backtest reruns they ask for are left out; one runs at the end.)
        let mut stopped = false;
        while let Some(action) = block_on(events.next()) {
            let Action::Output(message) = action else {
                continue;
            };
            let _ = app.update(message);
            let bars = app.tile_chart(0).unwrap().data().candles.len();
            if bars == 1_500 + replay.len() && !stopped {
                let _ = app.update(Message::Menu(MenuMessage::StopLive));
                assert_eq!(app.tile_live(0).unwrap().state, LiveState::Stopping);
                stopped = true;
            }
        }

        let live = app.tile_live(0).unwrap();
        assert_eq!(live.state, LiveState::Stopped(None));
        assert_eq!(live.since, Some(replay[0].ts));
        assert!(live.fills.len() >= 3, "{} fills", live.fills.len());
        assert!(
            live.book.position.abs() < 1e-9,
            "stopping closed the position"
        );
        assert_eq!(live.book.fills, live.fills.len());

        // The chart has the new bars, and its markers from the session start are the session's.
        let chart = app.tile_chart(0).unwrap();
        let markers: Vec<_> = chart
            .trades()
            .expect("trades")
            .markers
            .iter()
            .filter(|m| m.bar >= 1_500)
            .map(|m| (chart.data().candles[m.bar].ts, m.price))
            .collect();
        let fills: Vec<_> = live.fills.iter().map(|f| (f.bar, f.price)).collect();
        assert_eq!(markers, fills);

        finish_backtest(&mut app);
        let Run::Done(report) = run_state(&app) else {
            panic!()
        };
        assert_eq!(
            report.equity.len(),
            1_580,
            "the backtest covers the live bars too"
        );
    }

    #[test]
    fn new_settings_end_the_session() {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        finish_backtest(&mut app);
        app.rehearse_live(more_bars(1_000), Duration::from_millis(50));
        let _ = app.update(Message::OpenReport(0));
        let _events = app.update(Message::Menu(MenuMessage::GoLive(LiveMode::Paper)));
        assert!(app.tile_live(0).unwrap().is_active());

        let _ = app.update(Message::Menu(MenuMessage::StrategyParamChanged(
            0,
            "8".into(),
        )));
        assert!(
            app.tile_live(0).is_none(),
            "the session ran the old settings"
        );
    }
}

mod panel {
    use std::time::{Duration, Instant};

    use persephone::backtest::{
        BlockLength, BlockRun, History, OptimizeRun, RunColumn, ScoreBy, TestKind,
        calendar::{Calendar, Level},
        optimize::{OptimizeResult, Outcome},
        robustness::round_trips,
    };
    use persephone_engine::Direction;

    use super::*;

    fn panel(app: &mut Persephone, message: PanelMessage) {
        let _ = app.update(Message::Backtest(message));
    }

    fn shown(app: &Persephone) -> Option<usize> {
        app.tile_chart(0).unwrap().playhead().map(|p| p.bars)
    }

    #[test]
    fn the_backtest_panel_plays_a_finished_backtest_back() {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        finish_backtest(&mut app);

        // From the strategy report.
        let _ = app.update(Message::OpenReport(0));
        menu(&mut app, MenuMessage::OpenBacktest);
        assert!(app.popup().is_none());
        assert_eq!(app.panel().map(|p| p.tile), Some(0));
        assert_eq!(shown(&app), None, "the whole result until it plays");

        // At Max a whole span plays in two seconds: halfway after one.
        panel(&mut app, PanelMessage::Speed(1.0));
        panel(&mut app, PanelMessage::Play);
        assert!(app.is_animating(), "frames come while it plays");
        assert_eq!(shown(&app), Some(0), "from the first bar");
        let now = Instant::now();
        let _ = app.update(Message::Tick(now));
        let _ = app.update(Message::Tick(now + Duration::from_secs(1)));
        let half = shown(&app).unwrap();
        assert!((740..=760).contains(&half), "{half} of 1,500 bars");
        let _ = app.update(Message::Tick(now + Duration::from_secs(3)));
        assert_eq!(shown(&app), Some(1_500));
        assert!(!app.panel().unwrap().playback.playing, "stopped at the end");

        panel(&mut app, PanelMessage::Restart);
        assert_eq!(shown(&app), Some(0));
        panel(&mut app, PanelMessage::Step);
        panel(&mut app, PanelMessage::Step);
        assert_eq!(shown(&app), Some(2), "a bar at a time");
        let hundredth_close = 100 * Timeframe::H1.nanos();
        panel(&mut app, PanelMessage::Seek(hundredth_close));
        assert_eq!(shown(&app), Some(100));

        panel(&mut app, PanelMessage::Close);
        assert!(app.panel().is_none());
        assert_eq!(shown(&app), None, "the whole chart again");
    }

    #[test]
    fn ctrl_b_opens_and_closes_the_panel() {
        let mut app = app();
        // It opens without a strategy too, and says what's missing.
        let _ = app.update(Message::Key(Action::Backtest));
        assert_eq!(app.panel().map(|p| p.tile), Some(0));
        let _ = app.view();
        assert!(app.tile_strategy(0).is_none());

        // Its dropdown picks the strategy, without going through the chart's menu.
        panel(
            &mut app,
            PanelMessage::Strategy(StrategyChoice {
                kind: StrategyKind::BuiltIn(BuiltInStrategy::EmaCross),
                name: "EMA Cross".into(),
            }),
        );
        assert!(app.tile_strategy(0).is_some());
        assert_eq!(app.panel().map(|p| p.tile), Some(0), "the panel stays open");
        finish_backtest(&mut app);
        panel(&mut app, PanelMessage::Speed(1.0));
        panel(&mut app, PanelMessage::Play);
        assert_eq!(shown(&app), Some(0));

        let _ = app.update(Message::Key(Action::Backtest));
        assert!(app.panel().is_none());
        assert_eq!(shown(&app), None, "closing it shows the whole chart again");
    }

    #[test]
    fn the_right_click_menu_opens_the_panel() {
        let mut app = app();
        let _ = app.update(Message::Chart(
            0,
            ChartMessage::RightClicked {
                at: Point::new(200.0, 200.0),
            },
        ));
        menu(&mut app, MenuMessage::OpenBacktest);
        assert!(app.popup().is_none());
        assert_eq!(app.panel().map(|p| p.tile), Some(0));
    }

    #[test]
    fn plugins_are_added_from_the_search_and_taken_off_as_tags() {
        let mut app = app();
        let _ = app.update(Message::Key(Action::Backtest));
        let offered = |app: &Persephone| app.panel().unwrap().plugin_search.options().len();
        assert_eq!(offered(&app), 7);

        panel(&mut app, PanelMessage::AddPlugin("Fill model".into()));
        panel(&mut app, PanelMessage::AddPlugin("Fill model".into()));
        panel(&mut app, PanelMessage::AddPlugin("Holding costs".into()));
        assert_eq!(offered(&app), 5, "what's on is no longer offered");
        let _ = app.view();

        panel(&mut app, PanelMessage::RemovePlugin("Fill model".into()));
        assert_eq!(offered(&app), 6);
    }

    /// Draws a frame and delivers what the panel measured; returns the settings' height.
    fn measure(app: &mut Persephone) -> f32 {
        let messages: Vec<Message> = {
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                Size::new(1400.0, 900.0),
                app.view(),
            );
            ui.simulate([iced::Event::Window(iced::window::Event::RedrawRequested(
                Instant::now(),
            ))]);
            ui.into_messages().collect()
        };
        for message in messages {
            if matches!(message, Message::Backtest(_)) {
                let _ = app.update(message);
            }
        }
        app.panel().unwrap().settings_height
    }

    #[test]
    fn the_panel_is_always_tall_enough_for_its_settings() {
        let mut app = app();
        let _ = app.update(Message::Key(Action::Backtest));
        // Dragged as short as it goes.
        panel(&mut app, PanelMessage::Resize(0.0));
        let settings = measure(&mut app);
        assert!(settings > 100.0, "measured {settings}");
        let height = app.panel().unwrap().height_in(900.0);
        assert!(height >= settings, "{height} for {settings} of settings");

        // Tags can take more lines, and the panel grows with them.
        for (name, _) in persephone::backtest::PLUGIN_SLOTS {
            panel(&mut app, PanelMessage::AddPlugin(name.into()));
        }
        let taller = measure(&mut app);
        assert!(taller > settings, "{taller} with every tag");
        assert!(app.panel().unwrap().height_in(900.0) >= taller);

        // The charts keep some room in a window too short for it all.
        assert!(app.panel().unwrap().height_in(300.0) <= 150.0);
    }

    #[test]
    fn settings_edits_rerun_the_backtest() {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        finish_backtest(&mut app);
        let _ = app.update(Message::OpenReport(0));
        menu(&mut app, MenuMessage::OpenBacktest);

        panel(&mut app, PanelMessage::Edit(Edit::Direction(true)));
        assert!(
            matches!(run_state(&app), Run::Done(_)),
            "the last result shows meanwhile"
        );
        let (_, request) = app.backtest_request(0).unwrap();
        assert_eq!(request.rules.direction, Direction::LongOnly);

        panel(&mut app, PanelMessage::Input(Field::Slippage, "3".into()));
        panel(&mut app, PanelMessage::Submit(Field::Slippage));
        assert_eq!(
            app.backtest_request(0).unwrap().1.sim.slippage_ticks,
            0,
            "still off"
        );
        panel(&mut app, PanelMessage::Edit(Edit::Slippage(true)));
        assert_eq!(app.backtest_request(0).unwrap().1.sim.slippage_ticks, 3);

        panel(&mut app, PanelMessage::Input(Field::Capital, "nope".into()));
        panel(&mut app, PanelMessage::Submit(Field::Capital));
        assert!(app.panel().unwrap().error.is_some());
        assert_eq!(
            app.backtest_request(0).unwrap().1.starting_balance,
            100_000.0
        );

        panel(&mut app, PanelMessage::Edit(Edit::Stock(false)));
        let (_, request) = app.backtest_request(0).unwrap();
        assert_eq!(request.sim.slippage_ticks, 0, "stock settings off");
        assert_eq!(request.rules.direction, Direction::Both);
        finish_backtest(&mut app);
        assert!(matches!(run_state(&app), Run::Done(_)));
    }

    fn test_settings(app: &Persephone) -> &persephone::backtest::Settings {
        app.tile_backtest(0).unwrap()
    }

    #[test]
    fn monte_carlo_draws_the_backtests_trades_and_starts_over_when_it_changes() {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        finish_backtest(&mut app);
        let _ = app.update(Message::Key(Action::Backtest));

        panel(&mut app, PanelMessage::Test(TestKind::MonteCarlo));
        assert_eq!(test_settings(&app).test, TestKind::MonteCarlo);
        app.monte_carlo_now();
        let first = app
            .panel()
            .unwrap()
            .tests
            .monte_carlo
            .clone()
            .expect("the draws")
            .expect("enough trades");
        assert_eq!(first.runs, 1_000);
        assert!(first.low <= first.median && first.median <= first.high);

        // Only the backtest plays back.
        panel(&mut app, PanelMessage::Play);
        assert!(!app.panel().unwrap().playback.playing);

        // The same inputs give the same results; asking again gives others.
        panel(&mut app, PanelMessage::Input(Field::Ruin, "40".into()));
        panel(&mut app, PanelMessage::Submit(Field::Ruin));
        assert!(app.panel().unwrap().tests.monte_carlo.is_none());
        app.monte_carlo_now();
        let same = app.panel().unwrap().tests.monte_carlo.clone().unwrap();
        assert_eq!(same.unwrap().paths, first.paths);
        panel(&mut app, PanelMessage::RunAgain);
        app.monte_carlo_now();
        let other = app.panel().unwrap().tests.monte_carlo.clone().unwrap();
        assert_ne!(other.unwrap().paths, first.paths);

        // A changed backtest clears them, and its result draws them anew.
        panel(&mut app, PanelMessage::Edit(Edit::Slippage(true)));
        assert!(app.panel().unwrap().tests.monte_carlo.is_none());
        finish_backtest(&mut app);
        assert!(app.panel().unwrap().tests.monte_carlo_run.is_some());
        app.monte_carlo_now();
        assert!(app.panel().unwrap().tests.monte_carlo.is_some());
    }

    #[test]
    fn random_blocks_backtest_stretches_of_the_chart_and_clear_when_it_changes() {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        finish_backtest(&mut app);
        let _ = app.update(Message::Key(Action::Backtest));

        panel(&mut app, PanelMessage::Test(TestKind::RandomBlocks));
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        panel(&mut app, PanelMessage::Input(Field::Blocks, "6".into()));
        panel(&mut app, PanelMessage::Submit(Field::Blocks));
        assert_eq!(test_settings(&app).blocks, 6);
        assert!(matches!(run_state(&app), Run::Done(_)), "no rerun for that");

        app.blocks_now();
        let BlockRun::Done(summary) = &app.panel().unwrap().tests.blocks else {
            panic!("{:?}", app.panel().unwrap().tests.blocks);
        };
        assert_eq!(summary.paths.len(), 6);
        // A week of hours, from the same capital.
        assert_eq!(summary.paths[0].len(), 7 * 24);
        assert!((summary.paths[0][0] - 100_000.0).abs() < 1_000.0);
        assert!(summary.worst <= summary.median && summary.median <= summary.best);

        // Changing a setting takes them away.
        panel(&mut app, PanelMessage::Edit(Edit::Slippage(true)));
        assert!(matches!(app.panel().unwrap().tests.blocks, BlockRun::Idle));

        // A block too long for the bars says so.
        finish_backtest(&mut app);
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Quarter));
        panel(&mut app, PanelMessage::RunBlocks);
        let BlockRun::Failed(error) = &app.panel().unwrap().tests.blocks else {
            panic!("a refusal");
        };
        assert!(error.contains("3-month blocks need 91"), "{error}");

        // A batch that's stopped is told to, and its late result goes nowhere.
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        panel(&mut app, PanelMessage::RunBlocks);
        let BlockRun::Running {
            run, total, stop, ..
        } = &app.panel().unwrap().tests.blocks
        else {
            panic!("a batch running");
        };
        let (run, stopping) = (*run, std::sync::Arc::clone(&stop.0));
        assert_eq!(*total, 6);
        let _ = app.update(Message::BlocksProgress(run, 2));
        assert!(matches!(
            app.panel().unwrap().tests.blocks,
            BlockRun::Running { done: 2, .. }
        ));
        panel(&mut app, PanelMessage::StopBlocks);
        assert!(stopping.load(std::sync::atomic::Ordering::Relaxed));
        let _ = app.update(Message::BlocksDone(run, Err("Stopped.".into())));
        assert!(matches!(app.panel().unwrap().tests.blocks, BlockRun::Idle));
    }

    #[test]
    fn the_tests_settings_fit_in_the_panel_too() {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        let _ = app.update(Message::Key(Action::Backtest));
        panel(&mut app, PanelMessage::Resize(0.0));
        let backtest = measure(&mut app);
        panel(&mut app, PanelMessage::Test(TestKind::RandomBlocks));
        let blocks = measure(&mut app);
        assert!(blocks > backtest, "{blocks} against {backtest}");
        assert!(app.panel().unwrap().height_in(900.0) >= blocks);
    }

    fn ready_panel(test: TestKind) -> Persephone {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        finish_backtest(&mut app);
        let _ = app.update(Message::Key(Action::Backtest));
        panel(&mut app, PanelMessage::Test(test));
        app
    }

    fn found(app: &Persephone) -> Arc<OptimizeResult> {
        match &app.panel().unwrap().tests.optimize {
            OptimizeRun::Done(found) => Arc::clone(found),
            other => panic!("{other:?}"),
        }
    }

    fn set_range(app: &mut Persephone, slot: usize, min: &str, max: &str, step: &str) {
        panel(app, PanelMessage::Input(Field::Min(slot), min.into()));
        panel(app, PanelMessage::Submit(Field::Min(slot)));
        panel(app, PanelMessage::Input(Field::Max(slot), max.into()));
        panel(app, PanelMessage::Submit(Field::Max(slot)));
        panel(app, PanelMessage::Input(Field::Step(slot), step.into()));
        panel(app, PanelMessage::Submit(Field::Step(slot)));
    }

    #[test]
    fn the_panel_says_why_random_blocks_cant_run() {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        finish_backtest(&mut app);
        let _ = app.update(Message::Key(Action::Backtest));
        assert_eq!(app.test_problem(0), None, "nothing to say about a backtest");
        panel(&mut app, PanelMessage::Test(TestKind::RandomBlocks));
        assert_eq!(app.test_problem(0), None, "62 days fit 1-month blocks");

        // Too long for the bars: it says by how much, and offers to widen.
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Quarter));
        let problem = app.test_problem(0).expect("a problem");
        assert!(problem.message.contains("covers 62 days"), "{problem:?}");
        assert!(
            problem.message.contains("3-month blocks need 91"),
            "{problem:?}"
        );
        assert!(problem.widen);

        // Without a strategy, pressing Run isn't silent either.
        let _ = app.update(Message::Chart(
            0,
            ChartMessage::RightClicked {
                at: Point::new(200.0, 200.0),
            },
        ));
        menu(&mut app, MenuMessage::RemoveStrategy);
        let problem = app.test_problem(0).expect("a problem");
        assert_eq!(problem.message, "Pick a strategy first.");
        panel(&mut app, PanelMessage::RunBlocks);
        assert!(matches!(
            &app.panel().unwrap().tests.blocks,
            BlockRun::Failed(error) if error == "Pick a strategy first."
        ));
    }

    #[test]
    fn widen_span_reaches_back_far_enough_for_the_blocks() {
        let mut app = ready_panel(TestKind::RandomBlocks);
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Quarter));
        let hour = Timeframe::H1.nanos();
        let _ = app.update(Message::History(
            0,
            (None, "BTCUSDT".into()),
            Ok(Arc::new(History {
                first: 0,
                last: 20_000 * hour,
                closes: Vec::new(),
            })),
        ));
        panel(&mut app, PanelMessage::WidenSpan);
        let (from, to) = app.tile_span(0).expect("a span now");
        assert!(to - from >= 91 * 24 * hour, "{from} to {to}");
    }

    #[test]
    fn typed_values_count_when_run_is_pressed() {
        let mut app = ready_panel(TestKind::RandomBlocks);
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        panel(&mut app, PanelMessage::Input(Field::Blocks, "7".into()));
        panel(&mut app, PanelMessage::RunBlocks);
        assert_eq!(app.tile_backtest(0).unwrap().blocks, 7);
        assert!(matches!(
            app.panel().unwrap().tests.blocks,
            BlockRun::Running { total: 7, .. }
        ));

        // And one that's no good says so, without starting anything.
        panel(&mut app, PanelMessage::StopBlocks);
        panel(&mut app, PanelMessage::Input(Field::Blocks, "1".into()));
        panel(&mut app, PanelMessage::RunBlocks);
        assert!(
            app.panel()
                .unwrap()
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Blocks"))
        );
        assert!(matches!(app.panel().unwrap().tests.blocks, BlockRun::Idle));
    }

    #[test]
    fn bad_ranges_and_too_many_backtests_are_refused_before_running() {
        let mut app = ready_panel(TestKind::Optimize);
        set_range(&mut app, 0, "20", "10", "1");
        assert!(
            app.test_problem(0)
                .unwrap()
                .message
                .contains("Max must be above Min")
        );
        set_range(&mut app, 0, "10", "20", "0");
        assert!(
            app.test_problem(0)
                .unwrap()
                .message
                .contains("Step must be above zero")
        );
        panel(&mut app, PanelMessage::RunOptimize);
        assert!(matches!(
            app.panel().unwrap().tests.optimize,
            OptimizeRun::Failed(_)
        ));

        set_range(&mut app, 0, "1", "100", "1");
        panel(&mut app, PanelMessage::Variable(1, "Slow".into()));
        set_range(&mut app, 1, "1", "100", "1");
        let problem = app.test_problem(0).unwrap();
        assert!(problem.message.contains("10000 backtests"), "{problem:?}");
        panel(&mut app, PanelMessage::Variable(1, "None".into()));
        assert_eq!(app.test_problem(0), None);
    }

    #[test]
    fn optimizing_one_variable_ranks_its_values_and_keeps_some_bars_unseen() {
        let mut app = ready_panel(TestKind::Optimize);
        // The strategy's first variable is ready to go.
        panel(&mut app, PanelMessage::Variable(0, "Fast".into()));
        set_range(&mut app, 0, "6", "14", "2");
        app.optimize_now();
        let result = found(&app);
        assert_eq!(result.grid.first.values, vec![6.0, 8.0, 10.0, 12.0, 14.0]);
        assert!(result.grid.second.is_none());
        assert_eq!(result.outcomes.len(), 5);
        assert_eq!(result.top.len(), 5);
        assert!(result.held_out);
        assert!(
            result.top.iter().all(|r| r.unseen.is_some()),
            "{:?}",
            result.top
        );
    }

    #[test]
    fn the_unseen_part_warms_up_without_trading_and_starts_from_the_same_capital() {
        let mut app = ready_panel(TestKind::Optimize);
        panel(&mut app, PanelMessage::Variable(0, "Fast".into()));
        let job = app.optimize_job().expect("a job");
        let split = job.split.expect("some bars kept back");
        assert_eq!(split, 1_050, "70% of 1,500");
        let request = job.unseen_request(0, split).unwrap();
        assert_eq!(
            request.candles.len(),
            500 + 450,
            "warm-up bars from the 70% part"
        );
        assert_eq!(request.candles[500].ts, job.candles[split].ts);
        assert_eq!(request.trade_from, job.candles[split].ts);
        assert_eq!(request.starting_balance, job.request.starting_balance);
        let report = persephone_engine::run_backtest(&request).unwrap();
        assert!(
            report.fills.iter().all(|f| f.bar >= 500),
            "no trades warming up"
        );
        assert_eq!(report.equity[499], request.starting_balance);
    }

    #[test]
    fn a_value_that_fails_shows_as_grey_without_ending_the_batch() {
        let mut app = ready_panel(TestKind::Optimize);
        panel(&mut app, PanelMessage::Variable(0, "Fast".into()));
        set_range(&mut app, 0, "10", "20", "5");
        panel(&mut app, PanelMessage::Variable(1, "Slow".into()));
        set_range(&mut app, 1, "15", "25", "5");
        app.optimize_now();
        let result = found(&app);
        assert_eq!(result.outcomes.len(), 9);
        // Fast must be below Slow, so 15/15 and 20/15 and 20/20 can't run.
        let cell = result.grid.cell_of(&[20.0, 15.0]).unwrap();
        assert!(
            matches!(result.outcomes[cell], Outcome::Failed(_)),
            "{:?}",
            result.outcomes[cell]
        );
        let good = result.grid.cell_of(&[10.0, 25.0]).unwrap();
        assert!(result.outcomes[good].score().is_some());
        assert!(
            result
                .top
                .iter()
                .all(|r| result.outcomes[r.cell].score().is_some())
        );
    }

    #[test]
    fn every_score_gives_results() {
        for score in [
            ScoreBy::Backtest,
            ScoreBy::MonteCarlo,
            ScoreBy::RandomBlocks,
        ] {
            let mut app = ready_panel(TestKind::Optimize);
            panel(&mut app, PanelMessage::Variable(0, "Fast".into()));
            set_range(&mut app, 0, "8", "12", "2");
            panel(&mut app, PanelMessage::ScoreBy(score));
            // This market's slow cycle makes few trades, so Monte Carlo needs all its bars.
            panel(
                &mut app,
                PanelMessage::HoldOut(score != ScoreBy::MonteCarlo),
            );
            panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
            panel(&mut app, PanelMessage::Input(Field::Blocks, "3".into()));
            panel(&mut app, PanelMessage::Submit(Field::Blocks));
            app.optimize_now();
            let result = found(&app);
            assert_eq!(result.score, score);
            assert_eq!(result.outcomes.len(), 3);
            assert!(!result.top.is_empty(), "{score:?}: {:?}", result.outcomes);
        }
    }

    #[test]
    fn every_value_gets_the_same_blocks() {
        let mut app = ready_panel(TestKind::Optimize);
        panel(&mut app, PanelMessage::ScoreBy(ScoreBy::RandomBlocks));
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        let job = app.optimize_job().expect("a job");
        assert_eq!(job.blocks.len(), 50);
        let split = job.split.unwrap();
        assert!(
            job.blocks.iter().all(|b| b.end <= split),
            "none in the unseen bars"
        );
        assert_eq!(job.total(), job.grid.cells() * 50 + 5);
    }

    #[test]
    fn applying_a_result_keeps_the_results_and_can_be_undone() {
        let mut app = ready_panel(TestKind::Optimize);
        panel(&mut app, PanelMessage::Variable(0, "Fast".into()));
        set_range(&mut app, 0, "6", "14", "2");
        app.optimize_now();
        let fast = |app: &Persephone| app.tile_strategy(0).unwrap().0.params[0];
        assert_eq!(fast(&app), 10.0);

        panel(&mut app, PanelMessage::Apply(4));
        assert_eq!(fast(&app), 14.0);
        finish_backtest(&mut app);
        assert!(matches!(
            app.panel().unwrap().tests.optimize,
            OptimizeRun::Done(_)
        ));
        assert!(matches!(run_state(&app), Run::Done(_)), "the chart reran");

        // Trying another still goes back to the first original.
        panel(&mut app, PanelMessage::Apply(0));
        assert_eq!(fast(&app), 6.0);
        panel(&mut app, PanelMessage::Revert);
        assert_eq!(fast(&app), 10.0);
        assert!(matches!(
            app.panel().unwrap().tests.optimize,
            OptimizeRun::Done(_)
        ));
        assert_eq!(app.panel().unwrap().tests.original, None);

        // Any other change takes the results away.
        panel(&mut app, PanelMessage::Edit(Edit::Slippage(true)));
        assert!(matches!(
            app.panel().unwrap().tests.optimize,
            OptimizeRun::Idle
        ));
    }

    #[test]
    fn cancelling_stops_the_workers_and_drops_the_late_result() {
        let mut app = ready_panel(TestKind::Optimize);
        panel(&mut app, PanelMessage::RunOptimize);
        let OptimizeRun::Running { run, stop, .. } = &app.panel().unwrap().tests.optimize else {
            panic!("running");
        };
        let (run, stopping) = (*run, Arc::clone(&stop.0));
        let _ = app.update(Message::OptimizeProgress(run, 3));
        assert!(matches!(
            app.panel().unwrap().tests.optimize,
            OptimizeRun::Running { done: 3, .. }
        ));
        panel(&mut app, PanelMessage::StopOptimize);
        assert!(stopping.load(std::sync::atomic::Ordering::Relaxed));
        let _ = app.update(Message::OptimizeDone(run, Err("Stopped.".into())));
        assert!(matches!(
            app.panel().unwrap().tests.optimize,
            OptimizeRun::Idle
        ));
    }

    #[test]
    fn a_variable_the_strategy_no_longer_has_is_replaced() {
        let mut app = ready_panel(TestKind::Optimize);
        panel(&mut app, PanelMessage::Variable(1, "Slow".into()));
        set_strategy(&mut app, BuiltInStrategy::DonchianBreakout);
        finish_backtest(&mut app);
        let chosen = &app.tile_backtest(0).unwrap().optimize.variables;
        assert_eq!(chosen[0].as_ref().unwrap().name, "Length");
        assert!(chosen[1].is_none());
    }

    fn trades(app: &Persephone) -> Vec<persephone::backtest::robustness::RoundTrip> {
        let Run::Done(report) = run_state(app) else {
            panic!("the backtest hasn't finished");
        };
        round_trips(report)
    }

    #[test]
    fn the_calendar_adds_up_to_the_backtest() {
        let app = ready_panel(TestKind::Backtest);
        let Run::Done(report) = run_state(&app) else {
            panic!("the backtest hasn't finished");
        };
        let last = app.tile_chart(0).unwrap().data().candles.last().unwrap().ts;
        let calendar = Calendar::new(round_trips(report), last);
        let total: f64 = calendar
            .years()
            .iter()
            .map(|y| calendar.year(*y).profit)
            .sum();
        let made = report.equity.last().unwrap() - report.starting_balance;
        assert!((total - made).abs() < 1e-6, "{total} against {made}");
        let count: usize = calendar
            .years()
            .iter()
            .map(|y| calendar.year(*y).trades)
            .sum();
        assert_eq!(count, calendar.trades.len());
    }

    #[test]
    fn the_calendar_opens_level_by_level_and_draws() {
        let mut app = ready_panel(TestKind::Backtest);
        panel(&mut app, PanelMessage::ShowCalendar(true));
        assert!(app.panel().unwrap().calendar.shown);
        measure(&mut app);
        panel(&mut app, PanelMessage::CalendarLevel(Level::Month(1970, 1)));
        measure(&mut app);
        panel(
            &mut app,
            PanelMessage::CalendarLevel(Level::Day(1970, 1, 5)),
        );
        measure(&mut app);
        assert_eq!(app.panel().unwrap().calendar.level, Level::Day(1970, 1, 5));

        // A changed backtest has other trades, so it starts over at the top.
        panel(&mut app, PanelMessage::Edit(Edit::Slippage(true)));
        assert_eq!(app.panel().unwrap().calendar.level, Level::Years);
        assert!(app.panel().unwrap().calendar.shown);
    }

    #[test]
    fn replaying_a_trade_stops_just_before_it_and_plays_on_from_there() {
        let mut app = ready_panel(TestKind::Backtest);
        let trades = trades(&app);
        let index = trades.len() / 2;
        let entry = trades[index].entry_bar;
        assert!(entry > 20, "{entry}");

        panel(&mut app, PanelMessage::Speed(1.0));
        panel(&mut app, PanelMessage::Play);
        assert!(app.panel().unwrap().playback.playing);
        panel(&mut app, PanelMessage::ReplayTrade(index));

        let shown_now = shown(&app).expect("a playhead");
        assert_eq!(
            shown_now,
            entry - 10 + 1,
            "ten bars before it, that one closed"
        );
        assert!(!app.panel().unwrap().playback.playing, "paused, for Play");
        assert_eq!(app.panel().unwrap().calendar.replaying, Some(index));
        // The chart has come to it, however far away it was.
        let right = app.tile_chart(0).unwrap().viewport().right;
        assert!(
            (right - shown_now as f64).abs() < 20.0,
            "{right} for {shown_now}"
        );

        // Stepping on goes on from there, and reaches the trade.
        for _ in 0..11 {
            panel(&mut app, PanelMessage::Step);
        }
        assert!(
            shown(&app).unwrap() > entry,
            "{:?} for {entry}",
            shown(&app)
        );
    }

    #[test]
    fn replaying_an_early_trade_stays_inside_the_span() {
        let mut app = ready_panel(TestKind::Backtest);
        panel(&mut app, PanelMessage::ReplayTrade(0));
        assert!(shown(&app).is_some());
        // A trade that doesn't exist does nothing.
        let before = shown(&app);
        panel(&mut app, PanelMessage::ReplayTrade(1_000_000));
        assert_eq!(shown(&app), before);
    }

    #[test]
    fn only_the_backtest_replays_trades() {
        let mut app = ready_panel(TestKind::MonteCarlo);
        panel(&mut app, PanelMessage::ReplayTrade(0));
        assert_eq!(shown(&app), None);
    }

    /// Runs what a panel message hands back, delivering what it sends, as the app would.
    fn run_task(app: &mut Persephone, message: PanelMessage) {
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action as Run_, task::into_stream};
        let task = app.update(Message::Backtest(message));
        let mut events = into_stream(task).expect("a task");
        while let Some(action) = block_on(events.next()) {
            if let Run_::Output(message) = action {
                let _ = app.update(message);
            }
        }
    }

    #[test]
    fn pressing_run_delivers_random_blocks_through_its_task() {
        let mut app = ready_panel(TestKind::RandomBlocks);
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        panel(&mut app, PanelMessage::Input(Field::Blocks, "5".into()));
        run_task(&mut app, PanelMessage::RunBlocks);
        let BlockRun::Done(summary) = &app.panel().unwrap().tests.blocks else {
            panic!("{:?}", app.panel().unwrap().tests.blocks);
        };
        assert_eq!(summary.paths.len(), 5);
    }

    #[test]
    fn a_blocks_line_can_be_picked_and_the_chart_goes_to_it() {
        let mut app = ready_panel(TestKind::RandomBlocks);
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        panel(&mut app, PanelMessage::Input(Field::Blocks, "4".into()));
        panel(&mut app, PanelMessage::Submit(Field::Blocks));
        app.blocks_now();
        let BlockRun::Done(summary) = &app.panel().unwrap().tests.blocks else {
            panic!("no results");
        };
        let last_bar = summary.blocks[2].last_bar;
        assert!(summary.blocks.iter().all(|b| b.last_bar < 1_500));
        assert_eq!(summary.blocks.len(), summary.paths.len());
        panel(&mut app, PanelMessage::SelectLine(Some(2)));
        assert_eq!(app.panel().unwrap().tests.line, Some(2));
        let right = app.tile_chart(0).unwrap().viewport().right;
        assert!(
            (right - last_bar as f64).abs() < 20.0,
            "{right} for {last_bar}"
        );
        measure(&mut app);

        // Zooming stays until asked back, and a changed backtest starts over.
        panel(&mut app, PanelMessage::ZoomFan(Some((0.25, 0.5))));
        measure(&mut app);
        panel(&mut app, PanelMessage::Edit(Edit::Slippage(true)));
        assert_eq!(app.panel().unwrap().tests.line, None);
        assert_eq!(app.panel().unwrap().tests.zoom, None);
    }

    #[test]
    fn monte_carlos_lines_can_be_picked_including_the_backtests_own() {
        let mut app = ready_panel(TestKind::MonteCarlo);
        app.monte_carlo_now();
        let result = app
            .panel()
            .unwrap()
            .tests
            .monte_carlo
            .clone()
            .unwrap()
            .unwrap();
        panel(&mut app, PanelMessage::SelectLine(Some(result.best)));
        measure(&mut app);
        panel(&mut app, PanelMessage::SelectLine(Some(result.paths.len())));
        measure(&mut app);
        assert_eq!(app.panel().unwrap().tests.line, Some(result.paths.len()));
        // What really happened is one of the things it compares against.
        let Run::Done(report) = run_state(&app) else {
            panic!("no backtest");
        };
        let made = report.equity.last().unwrap() / report.starting_balance - 1.0;
        assert!((result.actual_stats.return_ - made).abs() < 1e-9);
    }

    #[test]
    fn stock_settings_fold_away_and_the_panel_needs_less_room() {
        let mut app = ready_panel(TestKind::Backtest);
        panel(&mut app, PanelMessage::Resize(0.0));
        let open = measure(&mut app);
        panel(&mut app, PanelMessage::StockOpen(false));
        let folded = measure(&mut app);
        assert!(folded < open, "{folded} against {open}");
        assert!(!app.tile_backtest(0).unwrap().stock_open);
        // Folding away changes nothing about the backtest.
        assert!(matches!(run_state(&app), Run::Done(_)));
    }

    #[test]
    fn random_blocks_runs_by_itself_like_monte_carlo() {
        let mut app = app();
        set_strategy(&mut app, BuiltInStrategy::EmaCross);
        finish_backtest(&mut app);
        let _ = app.update(Message::Key(Action::Backtest));
        assert!(matches!(app.panel().unwrap().tests.blocks, BlockRun::Idle));

        // Choosing it starts it, with results arriving without anything pressed.
        run_task(&mut app, PanelMessage::Test(TestKind::RandomBlocks));
        assert!(matches!(
            app.panel().unwrap().tests.blocks,
            BlockRun::Done(_)
        ));

        // A change starts it over.
        run_task(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        let BlockRun::Done(summary) = &app.panel().unwrap().tests.blocks else {
            panic!("no results for the new length");
        };
        assert!(summary.paths[0].len() <= 7 * 24);

        // But it stays stopped once cancelled, or if it can't run.
        panel(&mut app, PanelMessage::StopBlocks);
        assert!(matches!(app.panel().unwrap().tests.blocks, BlockRun::Idle));
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Quarter));
        assert!(
            matches!(app.panel().unwrap().tests.blocks, BlockRun::Idle),
            "too long for these bars"
        );

        // And a backtest that changes brings it back, not a blank pane.
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        panel(&mut app, PanelMessage::Edit(Edit::Slippage(true)));
        assert!(
            matches!(app.panel().unwrap().tests.blocks, BlockRun::Idle),
            "cleared"
        );
        finish_backtest(&mut app);
        assert!(matches!(
            app.panel().unwrap().tests.blocks,
            BlockRun::Running { .. }
        ));
    }

    #[test]
    fn something_half_typed_for_another_test_doesnt_stop_this_one() {
        let mut app = ready_panel(TestKind::Optimize);
        panel(&mut app, PanelMessage::Input(Field::Step(0), "oops".into()));
        panel(&mut app, PanelMessage::Test(TestKind::RandomBlocks));
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        run_task(&mut app, PanelMessage::RunBlocks);
        assert!(app.panel().unwrap().error.is_none());
        assert!(matches!(
            app.panel().unwrap().tests.blocks,
            BlockRun::Done(_)
        ));
    }

    #[test]
    fn picking_a_monte_carlo_line_lists_its_trades_with_a_way_back() {
        let mut app = ready_panel(TestKind::MonteCarlo);
        app.monte_carlo_now();
        let result = app
            .panel()
            .unwrap()
            .tests
            .monte_carlo
            .clone()
            .unwrap()
            .unwrap();
        let tests = |app: &Persephone| {
            let tests = &app.panel().unwrap().tests;
            (tests.line, tests.showing_trades)
        };

        panel(&mut app, PanelMessage::SelectLine(Some(result.worst)));
        assert_eq!(tests(&app), (Some(result.worst), true));
        measure(&mut app);

        // What it lists adds up to what the line says.
        let Run::Done(report) = run_state(&app) else {
            panic!("no backtest");
        };
        let trades = round_trips(report);
        let draws = result.draws(result.worst).unwrap();
        let made: f64 = draws.iter().map(|&i| trades[i].profit).sum();
        let expected = result.stats[result.worst].return_ * report.starting_balance;
        assert!((made - expected).abs() < 1e-6, "{made} against {expected}");

        // A trade can be seen where it really happened, even though the run's order isn't real.
        let exit = trades[draws[2]].exit_bar;
        panel(&mut app, PanelMessage::ShowTrade(2, exit));
        assert_eq!(app.panel().unwrap().tests.trade_row, Some(2));
        let right = app.tile_chart(0).unwrap().viewport().right;
        assert!((right - exit as f64).abs() < 30.0, "{right} for {exit}");

        // Back to the notable runs, with the line still picked.
        panel(&mut app, PanelMessage::ShowRuns);
        assert_eq!(tests(&app), (Some(result.worst), false));
        assert_eq!(app.panel().unwrap().tests.trade_row, None);
        measure(&mut app);

        // Clicking away from every line unpicks it.
        panel(&mut app, PanelMessage::SelectLine(None));
        assert_eq!(tests(&app), (None, false));
    }

    #[test]
    fn the_invert_switch_reruns_the_backtest_on_the_other_side() {
        let mut app = ready_panel(TestKind::Backtest);
        let before = trades(&app);
        assert!(!before.is_empty());

        panel(&mut app, PanelMessage::Edit(Edit::Invert(true)));
        assert!(app.backtest_request(0).unwrap().1.rules.invert);
        finish_backtest(&mut app);
        let after = trades(&app);
        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(&after) {
            assert_eq!(a.entry_bar, b.entry_bar);
            assert_ne!(a.long, b.long);
        }
        measure(&mut app);

        // The master switch turns it off with the rest.
        panel(&mut app, PanelMessage::Edit(Edit::Stock(false)));
        assert!(!app.backtest_request(0).unwrap().1.rules.invert);
    }

    #[test]
    fn monte_carlo_shows_every_run_as_a_table_and_two_tables_for_one() {
        let mut app = ready_panel(TestKind::MonteCarlo);
        app.monte_carlo_now();
        let result = app
            .panel()
            .unwrap()
            .tests
            .monte_carlo
            .clone()
            .unwrap()
            .unwrap();
        let tests = |app: &Persephone| {
            let tests = &app.panel().unwrap().tests;
            (tests.runs_table, tests.run, tests.line)
        };

        panel(&mut app, PanelMessage::ShowRunsTable(true));
        assert_eq!(tests(&app), (true, None, None));
        measure(&mut app);

        // Any run can be picked, drawn as a line or not, and the backtest itself.
        let hidden = (0..result.all_stats.len())
            .find(|&run| result.line_of_run(run).is_none())
            .expect("more runs than lines");
        for run in [hidden, result.all_stats.len()] {
            panel(&mut app, PanelMessage::SelectRun(Some(run)));
            assert_eq!(app.panel().unwrap().tests.run, Some(run));
            measure(&mut app);
        }

        // Picking a run leaves the table where it is, as it stays on screen beside the trades.
        panel(&mut app, PanelMessage::ScrollRuns(40));
        panel(&mut app, PanelMessage::SelectRun(Some(hidden)));
        assert_eq!(app.panel().unwrap().tests.runs_scroll, 40);
        panel(&mut app, PanelMessage::SelectRun(None));
        assert_eq!(tests(&app), (true, None, None));

        // Sorting flips on a second click, and goes back to the top.
        assert_eq!(
            app.panel().unwrap().tests.runs_sort,
            (RunColumn::Return, true)
        );
        panel(&mut app, PanelMessage::SortRuns(RunColumn::Return));
        assert_eq!(
            app.panel().unwrap().tests.runs_sort,
            (RunColumn::Return, false)
        );
        assert_eq!(app.panel().unwrap().tests.runs_scroll, 0);
        panel(&mut app, PanelMessage::SortRuns(RunColumn::Fell));
        assert_eq!(
            app.panel().unwrap().tests.runs_sort,
            (RunColumn::Fell, false)
        );
        measure(&mut app);

        // The same run stays picked going between lines and the table.
        panel(
            &mut app,
            PanelMessage::SelectRun(result.run_of_line(result.best)),
        );
        panel(&mut app, PanelMessage::ShowRunsTable(false));
        assert_eq!(tests(&app), (false, None, Some(result.best)));
        assert!(app.panel().unwrap().tests.showing_trades);
        panel(&mut app, PanelMessage::ShowRunsTable(true));
        assert_eq!(tests(&app).1, result.run_of_line(result.best));

        // Picked from the lines, it comes into sight in the table, however it's sorted.
        panel(&mut app, PanelMessage::ShowRunsTable(false));
        panel(&mut app, PanelMessage::SelectLine(Some(result.worst)));
        panel(&mut app, PanelMessage::ShowRunsTable(true));
        let worst = result.run_of_line(result.worst).unwrap();
        let mut rows = result.ranked_runs();
        persephone::backtest::robustness::sort_rows(
            &mut rows,
            app.panel().unwrap().tests.runs_sort,
        );
        let place = rows.iter().position(|r| r.run == worst).unwrap();
        assert_eq!(
            app.panel().unwrap().tests.runs_scroll,
            place.saturating_sub(2)
        );
        panel(&mut app, PanelMessage::SortRuns(RunColumn::Run));
        assert_eq!(
            app.panel().unwrap().tests.runs_scroll,
            worst.saturating_sub(2)
        );

        // A changed backtest starts over, but stays a table.
        panel(&mut app, PanelMessage::Edit(Edit::Slippage(true)));
        assert_eq!(tests(&app), (true, None, None));
    }

    #[test]
    fn random_blocks_shows_every_block_as_a_table_with_its_real_trades() {
        let mut app = ready_panel(TestKind::RandomBlocks);
        panel(&mut app, PanelMessage::BlockLength(BlockLength::Week));
        panel(&mut app, PanelMessage::Input(Field::Blocks, "6".into()));
        panel(&mut app, PanelMessage::Submit(Field::Blocks));
        app.blocks_now();
        let summary = match &app.panel().unwrap().tests.blocks {
            BlockRun::Done(summary) => Arc::clone(summary),
            other => panic!("{other:?}"),
        };
        assert_eq!(summary.trades.len(), 6);
        panel(&mut app, PanelMessage::ShowRunsTable(true));
        measure(&mut app);
        let rows = summary.ranked();
        assert_eq!(rows.len(), 6);
        assert!(rows[0].stats.return_ >= rows[5].stats.return_);

        // Each block's trades add up to it, and happened inside it on the chart.
        let candles = app.tile_chart(0).unwrap().data().candles.clone();
        let (with, block) = summary
            .trades
            .iter()
            .zip(&summary.blocks)
            .find(|(trades, _)| !trades.is_empty())
            .expect("a block with trades");
        let made: f64 = with.iter().map(|t| t.profit).sum();
        assert!((made / 100_000.0 - block.stats.return_).abs() < 1e-9);
        for trade in with {
            assert!(trade.entry_ts >= block.from, "traded while warming up");
            assert!(trade.exit_bar <= block.last_bar);
            // Its bar is the chart's: the fill came at that bar's close or just after.
            assert!(candles[trade.entry_bar].ts <= trade.entry_ts);
        }

        // Picking one shows its trades beside the table, and the chart goes there.
        let run = rows[0].run;
        panel(&mut app, PanelMessage::SelectRun(Some(run)));
        assert_eq!(app.panel().unwrap().tests.run, Some(run));
        let right = app.tile_chart(0).unwrap().viewport().right;
        let last = summary.blocks[run].last_bar as f64;
        assert!((right - last).abs() < 20.0, "{right} for {last}");
        measure(&mut app);

        // One with no trades at all still draws.
        if let Some(quiet) = summary.trades.iter().position(Vec::is_empty) {
            panel(&mut app, PanelMessage::SelectRun(Some(quiet)));
            measure(&mut app);
        }

        // The same block stays picked going back to lines.
        panel(&mut app, PanelMessage::SelectRun(Some(run)));
        panel(&mut app, PanelMessage::ShowRunsTable(false));
        assert_eq!(app.panel().unwrap().tests.line, Some(run));
        measure(&mut app);
    }

    fn results_dir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "persephone-results-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn save_result(
        dir: &std::path::Path,
        id: &str,
        strategy: serde_json::Value,
        settings: serde_json::Value,
    ) {
        let text = serde_json::json!({
            "id": id,
            "name": format!("Saved {id}"),
            "strategy": strategy,
            "settings": settings,
            "summary": {"net_return": 0.125},
            "saved_at": 1,
        });
        std::fs::write(dir.join(format!("{id}.json")), text.to_string()).unwrap();
    }

    fn choice(id: &str) -> PanelMessage {
        PanelMessage::Strategy(StrategyChoice {
            kind: StrategyKind::Plugin(format!("lab:{id}")),
            name: id.into(),
        })
    }

    #[test]
    fn strategies_lab_saved_can_be_backtested_and_replayed_in_the_app() {
        let mut app = ready_panel(TestKind::Backtest);
        let dir = results_dir();
        save_result(
            &dir,
            "tree",
            serde_json::json!({"logic": {
                "name": "Fast over slow",
                "params": {"fast": 5, "slow": 20},
                "long": {"crosses_above": [{"ema": {"length": "$fast"}}, {"ema": {"length": "$slow"}}]},
                "short": {"crosses_below": [{"ema": {"length": "$fast"}}, {"ema": {"length": "$slow"}}]},
            }, "params": {"fast": 6}}),
            serde_json::json!({"invert": true, "test": "monte-carlo"}),
        );
        let _ = app.use_results_dir(dir.clone());
        assert_eq!(app.lab_results().len(), 1);
        measure(&mut app);

        // Picking it puts it on the chart with the settings it was saved with.
        panel(&mut app, choice("tree"));
        let (spec, _) = app.tile_strategy(0).unwrap();
        assert_eq!(spec.info.name, "Fast over slow");
        assert_eq!(spec.params[0], 6.0, "its saved value");
        assert_eq!(spec.kind, StrategyKind::Plugin("lab:tree".into()));
        assert!(app.tile_backtest(0).unwrap().invert);
        assert_eq!(
            app.tile_backtest(0).unwrap().test,
            TestKind::Backtest,
            "which test shows stays the panel's"
        );

        // It runs like any strategy, and can be replayed.
        finish_backtest(&mut app);
        let made = trades(&app);
        assert!(!made.is_empty(), "it trades");
        panel(&mut app, PanelMessage::ReplayTrade(made.len() / 2));
        assert!(shown(&app).is_some());
    }

    #[test]
    fn a_saved_built_in_goes_on_as_itself_and_a_new_result_shows_up() {
        let mut app = ready_panel(TestKind::Backtest);
        let dir = results_dir();
        let _ = app.use_results_dir(dir.clone());
        assert!(app.lab_results().is_empty());

        save_result(
            &dir,
            "plain",
            serde_json::json!({"builtin": "rsi-reversion", "params": {"Length": 9}}),
            serde_json::Value::Null,
        );
        let _ = app.update(Message::LabResultsChanged);
        assert_eq!(app.lab_results()[0].name, "Saved plain");
        panel(&mut app, choice("plain"));
        let (spec, _) = app.tile_strategy(0).unwrap();
        assert_eq!(
            spec.kind,
            StrategyKind::BuiltIn(BuiltInStrategy::RsiReversion)
        );
        assert_eq!(spec.params[0], 9.0);

        // One that can't be used says why, and changes nothing.
        save_result(
            &dir,
            "gone",
            serde_json::json!({"plugin": "not-here"}),
            serde_json::Value::Null,
        );
        let _ = app.update(Message::LabResultsChanged);
        panel(&mut app, choice("gone"));
        assert!(
            app.panel()
                .unwrap()
                .error
                .as_deref()
                .unwrap()
                .contains("not-here")
        );
        assert_eq!(
            app.tile_strategy(0).unwrap().0.kind,
            StrategyKind::BuiltIn(BuiltInStrategy::RsiReversion)
        );
    }

    #[test]
    fn a_chart_saved_on_a_lab_strategy_picks_it_up_again() {
        use persephone::persist::{self, Saved, SavedStrategy, SavedTile};
        use persephone_tiling::Layout;

        let dir = results_dir();
        save_result(
            &dir,
            "tree",
            serde_json::json!({"logic": {
                "name": "Over its average",
                "params": {"n": 5},
                "long": {"gt": ["close", {"sma": {"length": "$n"}}]},
                "short": {"lt": ["close", {"sma": {"length": "$n"}}]},
            }}),
            serde_json::Value::Null,
        );
        let path = dir.join("layout.ron");
        persist::save(
            &path,
            &Saved {
                workspace: 1,
                others: Vec::new(),
                layout: Layout::new(),
                tiles: vec![SavedTile {
                    id: 0,
                    local: None,
                    symbol: "BTCUSDT".into(),
                    timeframe: "1h".into(),
                    indicators: Vec::new(),
                    strategy: Some(SavedStrategy {
                        kind: "plugin:lab:tree".into(),
                        params: vec![9.0, 25.0],
                        hidden: false,
                    }),
                    drawings_hidden: false,
                    volume_hidden: false,
                    span: None,
                    backtest: None,
                }],
            },
        )
        .unwrap();

        let (mut app, _loads) = Persephone::start(&[], Some(path));
        let (waiting, _) = app.tile_strategy(0).unwrap();
        assert!(
            waiting.info.params.is_empty(),
            "a placeholder until it's read"
        );
        assert_eq!(waiting.params, vec![9.0, 25.0], "values kept meanwhile");

        let _ = app.use_results_dir(dir.clone());
        let (spec, _) = app.tile_strategy(0).unwrap();
        assert_eq!(spec.info.name, "Over its average");
        assert_eq!(
            spec.params,
            vec![9.0, 25.0],
            "with the values it was saved at"
        );
    }
}
