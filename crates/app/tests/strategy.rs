//! Strategies on charts, driven through the same messages the UI sends. Backtests run on the
//! test thread here (the app would hand them to a worker) and come back as `BacktestDone`.

use std::{path::PathBuf, sync::Arc};

use iced::{Point, Size};
use persephone::{
    Message, Persephone, Run,
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
        ChartMessage::Clicked {
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
        ChartMessage::Clicked {
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
        ChartMessage::Clicked {
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
        ChartMessage::Clicked {
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
