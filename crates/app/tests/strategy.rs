//! Strategies on charts, driven through the same messages the UI sends. Backtests run on the
//! test thread here (the app would hand them to a worker) and come back as `BacktestDone`.

use std::{path::PathBuf, sync::Arc};

use iced::{Point, Size};
use persephone::{
    Message, Persephone, Run,
    backtest::{Edit, Field, PanelMessage},
    menu::{MenuMessage, Page, Popup},
};
use persephone_chart::{ChartData, ChartMessage};
use persephone_core::{
    Candle, Timeframe,
    indicator::BuiltIn,
    strategy::{BuiltInStrategy, StrategySpec},
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
}
