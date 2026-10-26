//! Plugins inside the app, using the real example plugins from `plugins/` (built for
//! wasm32-wasip2 once per test run).

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use iced::{Point, Size};
use persephone::{
    Message, Persephone, Run,
    menu::{MenuMessage, Page},
    persist::{self, Saved, SavedIndicator, SavedStrategy, SavedTile},
};
use persephone_chart::{ChartData, ChartMessage};
use persephone_core::{
    Candle, Timeframe,
    indicator::IndicatorKind,
    strategy::{BuiltInStrategy, StrategyKind, StrategySpec},
};
use persephone_plugin_host::{Host, Plugins};
use persephone_tiling::Layout;

fn built() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "--release", "--target", "wasm32-wasip2", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building the plugins failed");
        root.join("target/wasm32-wasip2/release")
    })
}

/// A plugins folder holding copies of the named plugins, loaded.
fn plugins(names: &[&str]) -> Plugins {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "persephone-app-plugins-{}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in names {
        std::fs::copy(
            built().join(format!("{name}.wasm")),
            dir.join(format!("{name}.wasm")),
        )
        .unwrap();
    }
    let loaded = Host::new().unwrap().load_dir(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
    loaded
}

fn app() -> Persephone {
    let candles = (0..1_200u64)
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
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::Clicked {
            at: Point::new(100.0, 100.0),
        },
    ));
    let _ = app.update(Message::Menu(message));
}

fn finish_backtest(app: &mut Persephone) {
    let (generation, request) = app.backtest_request(0).expect("a backtest to run");
    let result = persephone_engine::run_backtest(&request)
        .map(Arc::new)
        .map_err(|e| e.to_string());
    let _ = app.update(Message::BacktestDone(0, generation, result));
}

#[test]
fn plugin_indicators_join_the_menu_and_draw() {
    let mut app = app();
    let _ = app.set_plugins(plugins(&["sma", "zscore"]));
    assert_eq!(app.plugins().indicators.len(), 2);

    menu(&mut app, MenuMessage::AddPluginIndicator("sma".into()));
    menu(&mut app, MenuMessage::AddPluginIndicator("zscore".into()));
    let studies = app.tile_chart(0).unwrap().studies();
    assert_eq!(
        studies.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(),
        ["pSMA 20", "Z 50"]
    );
    assert!(
        studies
            .iter()
            .all(|s| s.error.is_none() && !s.lines[0].values.is_empty())
    );
    assert_eq!(studies[1].levels, vec![-2.0, 0.0, 2.0]);
}

#[test]
fn a_crashing_plugin_shows_its_panic_in_the_legend() {
    let mut app = app();
    let _ = app.set_plugins(plugins(&["panics"]));
    menu(&mut app, MenuMessage::AddPluginIndicator("panics".into()));
    let study = &app.tile_chart(0).unwrap().studies()[0];
    assert!(
        study.error.as_deref().unwrap_or_default().contains("boom"),
        "{:?}",
        study.error
    );
}

#[test]
fn a_plugin_strategy_backtests_like_its_built_in_twin() {
    let mut app = app();
    let _ = app.set_plugins(plugins(&["ema_cross"]));
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::Clicked {
            at: Point::new(5.0, 5.0),
        },
    ));
    let _ = app.update(Message::Menu(MenuMessage::Open(Page::Strategies)));
    let _ = app.update(Message::Menu(MenuMessage::SetPluginStrategy(
        "ema_cross".into(),
    )));
    assert!(matches!(
        app.tile_strategy(0).unwrap().0.kind,
        StrategyKind::Plugin(_)
    ));
    finish_backtest(&mut app);
    let Run::Done(plugin) = app.tile_strategy(0).unwrap().1.clone() else {
        panic!("plugin run failed")
    };

    menu(
        &mut app,
        MenuMessage::SetStrategy(BuiltInStrategy::EmaCross),
    );
    finish_backtest(&mut app);
    let Run::Done(built_in) = app.tile_strategy(0).unwrap().1.clone() else {
        panic!("built-in run failed")
    };

    assert!(!plugin.fills.is_empty());
    assert_eq!(plugin.fills, built_in.fills);
}

#[test]
fn a_missing_strategy_plugin_fails_its_backtest_clearly() {
    let mut app = app();
    let _ = app.set_plugins(plugins(&["ema_cross"]));
    menu(&mut app, MenuMessage::SetPluginStrategy("ema_cross".into()));
    let _ = app.set_plugins(Plugins::default()); // the file was deleted
    finish_backtest(&mut app);
    let Run::Failed(message) = app.tile_strategy(0).unwrap().1.clone() else {
        panic!("should fail")
    };
    assert!(message.contains("ema_cross"), "{message}");
}

#[test]
fn saved_plugins_wait_for_their_plugin_then_resolve() {
    let dir = std::env::temp_dir().join(format!("persephone-saved-plugins-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("layout.ron");
    persist::save(
        &path,
        &Saved {
            layout: Layout::new(),
            tiles: vec![SavedTile {
                id: 0,
                local: None,
                symbol: "BTCUSDT".into(),
                timeframe: "1h".into(),
                indicators: vec![SavedIndicator {
                    kind: "plugin:sma".into(),
                    params: vec![30.0],
                }],
                strategy: Some(SavedStrategy {
                    kind: "plugin:ema_cross".into(),
                    params: vec![8.0, 21.0, 25.0],
                }),
            }],
        },
    )
    .unwrap();

    let (mut app, _loads) = Persephone::start(&[], Some(path));
    let waiting = &app.tile_indicators(0).unwrap()[0];
    assert_eq!(waiting.kind, IndicatorKind::Plugin("sma".into()));
    assert_eq!(
        waiting.params,
        vec![30.0],
        "values kept while the plugin loads"
    );

    let _ = app.set_plugins(plugins(&["sma", "ema_cross"]));
    let resolved = &app.tile_indicators(0).unwrap()[0];
    assert_eq!(resolved.to_string(), "pSMA 30");
    let (strategy, _) = app.tile_strategy(0).unwrap();
    assert_eq!(strategy.info.name, "EMA Cross (plugin)");
    assert_eq!(strategy.params, vec![8.0, 21.0, 25.0]);
    assert!((strategy.size_fraction() - 0.25).abs() < 1e-12);
    assert_ne!(strategy, &StrategySpec::new(BuiltInStrategy::EmaCross));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ai_plugins_ask_before_calling_and_remember_the_answer() {
    let mut app = app();
    let _ = app.set_plugins(plugins(&["ai_probe", "ema_cross"]));
    assert!(app.plugins().strategies["ai_probe"].uses_ai);
    assert!(!app.plugins().strategies["ema_cross"].uses_ai);

    menu(&mut app, MenuMessage::SetPluginStrategy("ai_probe".into()));
    finish_backtest(&mut app);
    let Run::Done(report) = app.tile_strategy(0).unwrap().1.clone() else {
        panic!("the run failed")
    };
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.warning && n.text.contains("isn't allowed to ask")),
        "{:?}",
        report.notes
    );
    assert!(!app.plugins().strategies["ai_probe"].ai_allowed());

    let _ = app.update(Message::OpenReport(0));
    let _ = app.update(Message::Menu(MenuMessage::AllowAi(true)));
    assert!(app.plugins().strategies["ai_probe"].ai_allowed());
    assert!(
        matches!(app.tile_strategy(0).unwrap().1, Run::Running),
        "allowing reruns the backtest"
    );
    finish_backtest(&mut app);
    let Run::Done(report) = app.tile_strategy(0).unwrap().1.clone() else {
        panic!("the run failed")
    };
    // This test host has no AI client, so the questions go unanswered, and the report says so.
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.text.contains("got no answer") && n.text.contains("aren't set up")),
        "{:?}",
        report.notes
    );

    let _ = app.update(Message::Menu(MenuMessage::AllowAi(false)));
    assert!(!app.plugins().strategies["ai_probe"].ai_allowed());
}
