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
use persephone_engine::{
    BacktestRequest, EngineError, FeeRates, SimSettings, TradingRules, run_backtest,
};
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
        bars: Timeframe::H1.into(),
        candles: candles(1_500),
        strategy,
        plugin,
        ticks: None,
        starting_balance: 100_000.0,
        fees: FeeRates::BINANCE_SPOT,
        sim: SimSettings::default(),
        rules: TradingRules::default(),
        trade_from: 0,
        fill_bars: None,
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

// --- Models and language models ------------------------------------------------------------

mod services {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        path::PathBuf,
        sync::{Arc, Mutex},
        thread,
    };

    use persephone_core::{signal::SignalFactory, strategy::StrategyKind, strategy::StrategySpec};
    use persephone_engine::run_backtest;
    use persephone_ml::{
        ai::{Ai, Config},
        models::{Models, logistic_regression},
    };
    use persephone_plugin_host::{Host, Plugin, Services};

    use super::{built, candles, request};

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("persephone-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in Anthropic API: every question is answered LONG, model lookups say the model
    /// came out on 2026-04-01. Returns the base URL and the request lines it saw.
    fn anthropic() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = header.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = if line.starts_with("GET /v1/models/") {
                    r#"{"id":"claude-opus-5-5","created_at":"2026-04-01T00:00:00Z"}"#
                } else {
                    r#"{"content":[{"type":"text","text":"LONG"}],"stop_reason":"end_turn"}"#
                };
                log.lock().unwrap().push(line.trim_end().to_owned());
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, seen)
    }

    fn strategy(host: &Host, name: &str) -> Arc<persephone_plugin_host::StrategyPlugin> {
        match host.load(&built().join(format!("{name}.wasm"))).unwrap() {
            Plugin::Strategy(p) => p,
            Plugin::Indicator(_) => panic!("{name} is a strategy"),
        }
    }

    #[test]
    fn strategies_say_whether_they_ask_models() {
        let host = Host::new().unwrap();
        assert!(strategy(&host, "ai_probe").uses_ai);
        assert!(strategy(&host, "llm_daily").uses_ai);
        assert!(
            !strategy(&host, "ema_cross").uses_ai,
            "unused imports are left out of components"
        );
        assert!(!strategy(&host, "ml_trend").uses_ai);
    }

    #[test]
    fn indicators_run_models_from_the_folder() {
        let dir = temp("ml");
        std::fs::write(dir.join("probe.onnx"), logistic_regression(&[2.0], -1.0)).unwrap();
        let host =
            Host::with_services(Services::new(Some(Arc::new(Models::new(&dir))), None)).unwrap();
        let Plugin::Indicator(probe) = host.load(&built().join("ml_probe.wasm")).unwrap() else {
            panic!()
        };
        let data = candles(200);
        let values = probe.compute(&[], &data).unwrap();
        for (value, candle) in values[0].iter().zip(&data) {
            let expected = 1.0 / (1.0 + (-(2.0 * (candle.close / 10_000.0) as f32 - 1.0)).exp());
            assert!(
                (*value - f64::from(expected)).abs() < 1e-6,
                "{value} vs {expected}"
            );
        }

        let bare = Host::new().unwrap();
        let Plugin::Indicator(probe) = bare.load(&built().join("ml_probe.wasm")).unwrap() else {
            panic!()
        };
        assert!(
            probe
                .compute(&[], &data)
                .unwrap_err()
                .contains("models aren't available")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_model_driven_strategy_trades_and_explains_a_missing_model() {
        let dir = temp("ml-trend");
        let host =
            Host::with_services(Services::new(Some(Arc::new(Models::new(&dir))), None)).unwrap();
        let plugin = strategy(&host, "ml_trend");
        let spec = StrategySpec::from_info(
            StrategyKind::Plugin(plugin.id.clone()),
            Arc::clone(&plugin.info),
            &[55.0, 10.0],
        );
        let factory: Arc<dyn SignalFactory> = plugin;

        let error = run_backtest(&request(spec.clone(), Some(Arc::clone(&factory)))).unwrap_err();
        assert!(error.to_string().contains("models example"), "{error}");

        // Momentum: rising returns mean a rising close.
        std::fs::write(
            dir.join("trend.onnx"),
            logistic_regression(&[1.0, 1.0, 1.0, 1.0], 0.0),
        )
        .unwrap();
        let report = run_backtest(&request(spec, Some(factory))).unwrap();
        assert!(report.fills.len() > 2, "{} fills", report.fills.len());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn ai_host(url: &str, name: &str, max_new_calls: Option<u32>) -> (Host, PathBuf) {
        let dir = temp(name);
        let config = Config {
            anthropic_api_key: Some("test".into()),
            anthropic_base_url: Some(url.into()),
            max_new_calls,
            ..Config::default()
        };
        let ai = Ai::new(config, dir.join("cache")).unwrap();
        (
            Host::with_services(Services::new(None, Some(Arc::new(ai)))).unwrap(),
            dir,
        )
    }

    fn probe_run(host: &Host) -> persephone_engine::BacktestReport {
        let plugin = strategy(host, "ai_probe");
        // 1,500 bars, a question every 300: five questions.
        let spec = StrategySpec::from_info(
            StrategyKind::Plugin(plugin.id.clone()),
            Arc::clone(&plugin.info),
            &[300.0, 10.0],
        );
        let factory: Arc<dyn SignalFactory> = plugin;
        run_backtest(&request(spec, Some(factory))).unwrap()
    }

    #[test]
    fn language_models_need_permission_and_answers_are_recorded() {
        let (url, seen) = anthropic();
        let (host, dir) = ai_host(&url, "ai", None);

        // Not allowed yet: no calls, no trades, and the report says why.
        let refused = probe_run(&host);
        assert!(refused.fills.is_empty());
        assert!(
            seen.lock().unwrap().iter().all(|l| !l.starts_with("POST")),
            "{:?}",
            seen.lock().unwrap()
        );
        assert!(
            refused
                .notes
                .iter()
                .any(|n| n.warning && n.text.contains("isn't allowed to ask")),
            "{:?}",
            refused.notes
        );

        host.services().allow_ai("ai_probe", true);
        let first = probe_run(&host);
        assert_eq!(
            first.fills.len(),
            1,
            "the first LONG opens the position; the rest keep it"
        );
        let texts: Vec<&str> = first.notes.iter().map(|n| n.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.contains(
                "Asked claude-opus-5-5 5 times: 0 answered from recordings, 5 new calls"
            )),
            "{texts:?}"
        );
        let look_ahead = first
            .notes
            .iter()
            .find(|n| n.text.starts_with("Look-ahead"))
            .expect("a look-ahead warning");
        assert!(
            look_ahead.warning
                && look_ahead.text.contains("2026-04-01")
                && look_ahead.text.contains("2025-01"),
            "{}",
            look_ahead.text
        );
        let posts = |seen: &Arc<Mutex<Vec<String>>>| {
            seen.lock()
                .unwrap()
                .iter()
                .filter(|l| l.starts_with("POST"))
                .count()
        };
        assert_eq!(posts(&seen), 5);

        // The same backtest again: every answer from the recordings, nothing sent.
        let again = probe_run(&host);
        assert_eq!(again.fills, first.fills);
        assert!(
            again
                .notes
                .iter()
                .any(|n| n.text.contains("5 answered from recordings, 0 new calls")),
            "{:?}",
            again.notes
        );
        assert_eq!(posts(&seen), 5);
        let lookups = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("GET"))
            .count();
        assert_eq!(lookups, 1, "the release date is looked up once");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn each_backtest_has_a_budget_of_new_calls() {
        let (url, _) = anthropic();
        let (host, dir) = ai_host(&url, "budget", Some(2));
        host.services().allow_ai("ai_probe", true);
        let report = probe_run(&host);
        let texts: Vec<&str> = report.notes.iter().map(|n| n.text.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("2 times") && t.contains("2 new calls")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("limit of 2 new AI calls")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("3 questions got no answer")),
            "{texts:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
