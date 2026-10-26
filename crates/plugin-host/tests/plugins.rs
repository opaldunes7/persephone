//! Runs the real example plugins and the deliberately broken fixtures from `plugins/`, built
//! for wasm32-wasip2 once per test run.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use persephone_core::{
    Candle, InstrumentSpec, Timeframe,
    indicator::{BuiltIn, IndicatorSpec, OutputStyle, Placement},
    signal::SignalFactory,
    strategy::{BuiltInStrategy, StrategyKind, StrategySpec},
};
use persephone_engine::{BacktestRequest, EngineError, FeeRates, run_backtest};
use persephone_plugin_host::{Host, Plugin};

/// Builds the plugins workspace (once) and returns the folder with the `.wasm` files.
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

fn host() -> Host {
    Host::new().expect("the runtime starts")
}

fn load(host: &Host, name: &str) -> Plugin {
    host.load(&built().join(format!("{name}.wasm")))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn indicator(host: &Host, name: &str) -> Arc<persephone_plugin_host::IndicatorPlugin> {
    match load(host, name) {
        Plugin::Indicator(p) => p,
        Plugin::Strategy(_) => panic!("{name} should be an indicator"),
    }
}

fn candles(n: usize) -> Vec<Candle> {
    (0..n)
        .map(|i| {
            let p = 50_000.0 + 2_000.0 * (i as f64 / 40.0).sin() + 300.0 * (i as f64 / 7.0).cos();
            Candle {
                ts: 1_735_689_600_000_000_000 + i as u64 * Timeframe::H1.nanos(),
                open: p - 5.0,
                high: p + 20.0,
                low: p - 20.0,
                close: p + 5.0,
                volume: 10.0,
            }
        })
        .collect()
}

#[test]
fn indicators_describe_themselves() {
    let host = host();
    let sma = indicator(&host, "sma");
    assert_eq!(sma.id, "sma");
    assert_eq!(sma.info.short_name, "pSMA");
    assert_eq!(sma.info.placement, Placement::Overlay);
    assert_eq!(sma.info.params[0].name, "Length");
    assert_eq!(sma.info.params[0].default, 20.0);

    let z = indicator(&host, "zscore");
    assert_eq!(z.info.placement, Placement::Pane);
    assert_eq!(z.info.levels, vec![-2.0, 0.0, 2.0]);
    assert_eq!(z.info.outputs[0].style, OutputStyle::Line);
}

#[test]
fn the_sma_plugin_matches_the_built_in() {
    let host = host();
    let data = candles(2_000);
    let plugin = indicator(&host, "sma").compute(&[20.0], &data).unwrap();
    let builtin = persephone_engine::indicators::compute(
        &IndicatorSpec::with_params(BuiltIn::Sma, &[20.0]),
        &data,
    )
    .unwrap();

    for (i, (p, b)) in plugin[0].iter().zip(&builtin[0]).enumerate() {
        if b.is_nan() {
            assert!(p.is_nan(), "bar {i}: plugin has {p}, built-in has none");
        } else {
            assert!((p - b).abs() <= b.abs() * 1e-12, "bar {i}: {p} vs {b}");
        }
    }
}

#[test]
fn a_panicking_plugin_reports_its_message() {
    let host = host();
    let error = indicator(&host, "panics")
        .compute(&[], &candles(10))
        .unwrap_err();
    assert!(error.contains("boom after 10 bars"), "{error}");
    // The host is fine afterwards.
    assert!(
        indicator(&host, "sma")
            .compute(&[5.0], &candles(10))
            .is_ok()
    );
}

#[test]
fn a_spinning_plugin_is_stopped() {
    let host = host();
    let started = Instant::now();
    let error = indicator(&host, "spins")
        .compute(&[], &candles(10))
        .unwrap_err();
    assert!(error.contains("too long"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "stopped in {:?}",
        started.elapsed()
    );
}

#[test]
fn a_greedy_plugin_hits_the_memory_limit() {
    let host = host();
    let error = indicator(&host, "hungry")
        .compute(&[], &candles(10))
        .unwrap_err();
    assert!(
        error.contains("memory") || error.contains("alloc"),
        "{error}"
    );
}

#[test]
fn plugins_see_a_frozen_clock_and_seeded_randomness() {
    let host = host();
    let clock = indicator(&host, "clock");
    let first = clock.compute(&[], &candles(3)).unwrap();
    assert_eq!(
        first[0][0], 0.0,
        "the wall clock is stopped at the Unix epoch"
    );
    assert_eq!(first[1][0], 0.0, "no time passes inside a plugin");
    assert_eq!(
        first,
        clock.compute(&[], &candles(3)).unwrap(),
        "the same randomness every run"
    );
}

#[test]
fn unchanged_files_load_from_the_cache() {
    let host = host();
    let (Plugin::Indicator(a), Plugin::Indicator(b)) = (load(&host, "sma"), load(&host, "sma"))
    else {
        panic!("sma is an indicator");
    };
    assert!(Arc::ptr_eq(&a, &b));
}

#[test]
fn a_folder_loads_plugins_and_reports_broken_files() {
    let dir = std::env::temp_dir().join(format!("persephone-plugins-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["sma", "ema_cross"] {
        std::fs::copy(
            built().join(format!("{name}.wasm")),
            dir.join(format!("{name}.wasm")),
        )
        .unwrap();
    }
    std::fs::write(dir.join("broken.wasm"), b"not wasm").unwrap();
    std::fs::write(dir.join("notes.txt"), b"ignored").unwrap();

    let plugins = host().load_dir(&dir);
    assert_eq!(plugins.indicators.keys().collect::<Vec<_>>(), ["sma"]);
    assert_eq!(plugins.strategies.keys().collect::<Vec<_>>(), ["ema_cross"]);
    assert_eq!(plugins.errors.len(), 1);
    assert_eq!(plugins.errors[0].0, "broken.wasm");
    std::fs::remove_dir_all(dir).unwrap();
}

fn request(strategy: StrategySpec, plugin: Option<Arc<dyn SignalFactory>>) -> BacktestRequest {
    BacktestRequest {
        instrument: InstrumentSpec {
            symbol: "BTCUSDT".into(),
            venue: "BINANCE".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 5,
        },
        timeframe: Timeframe::H1,
        candles: candles(1_500),
        strategy,
        plugin,
        starting_balance: 100_000.0,
        fees: FeeRates::BINANCE_SPOT,
    }
}

fn ema_cross_plugin(host: &Host) -> Arc<persephone_plugin_host::StrategyPlugin> {
    match load(host, "ema_cross") {
        Plugin::Strategy(p) => p,
        Plugin::Indicator(_) => panic!("ema_cross should be a strategy"),
    }
}

#[test]
fn the_ema_cross_plugin_trades_exactly_like_the_built_in() {
    let host = host();
    let plugin = ema_cross_plugin(&host);
    assert_eq!(
        plugin.info.params.last().unwrap().name,
        "Size %",
        "the host adds sizing"
    );

    let spec = StrategySpec::from_info(
        StrategyKind::Plugin(plugin.id.clone()),
        Arc::clone(&plugin.info),
        &[10.0, 30.0, 10.0],
    );
    let factory: Arc<dyn SignalFactory> = plugin;
    let from_plugin = run_backtest(&request(spec, Some(factory))).unwrap();
    let built_in = run_backtest(&request(
        StrategySpec::with_params(BuiltInStrategy::EmaCross, &[10.0, 30.0, 10.0]),
        None,
    ))
    .unwrap();

    assert!(!built_in.fills.is_empty());
    assert_eq!(from_plugin.fills, built_in.fills);
    assert_eq!(from_plugin.equity, built_in.equity);
}

#[test]
fn a_plugin_strategy_can_refuse_its_parameters() {
    let host = host();
    let plugin = ema_cross_plugin(&host);
    let spec = StrategySpec::from_info(
        StrategyKind::Plugin(plugin.id.clone()),
        Arc::clone(&plugin.info),
        &[50.0, 30.0, 10.0],
    );
    let factory: Arc<dyn SignalFactory> = plugin;
    match run_backtest(&request(spec, Some(factory))) {
        Err(EngineError::Strategy(message)) => assert!(message.contains("Fast"), "{message}"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_plugin_strategy_needs_its_plugin() {
    let host = host();
    let plugin = ema_cross_plugin(&host);
    let spec = StrategySpec::from_info(
        StrategyKind::Plugin("ema_cross".into()),
        Arc::clone(&plugin.info),
        &[],
    );
    assert!(matches!(
        run_backtest(&request(spec, None)),
        Err(EngineError::InvalidRequest(_))
    ));
}
