//! Strategies in a process of their own: a stand-in process here, on a local socket, backtests
//! exactly like the same rule run in-process.

use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::Arc,
    thread,
};

use persephone_core::{
    Candle, InstrumentSpec, Timeframe,
    signal::{Signal, SignalFactory},
    strategy::{StrategyKind, StrategySpec},
};
use persephone_engine::{BacktestReport, BacktestRequest, EngineError, FeeRates, run_backtest};
use persephone_plugin_host::{Host, Plugins};
use serde_json::{Value, json};

/// Long when the close is above the close `lookback` bars ago, short when below.
#[derive(Debug, Default)]
struct Momentum {
    lookback: usize,
    closes: Vec<f64>,
}

impl Signal for Momentum {
    fn on_bar(&mut self, bar: &Candle) -> Result<Option<i8>, String> {
        self.closes.push(bar.close);
        let n = self.closes.len();
        if n <= self.lookback {
            return Ok(None);
        }
        let then = self.closes[n - 1 - self.lookback];
        Ok(Some(if bar.close > then { 1 } else { -1 }))
    }
}

#[derive(Debug)]
struct InProcess;

impl SignalFactory for InProcess {
    fn create(&self, params: &[f64]) -> Result<Box<dyn Signal>, String> {
        Ok(Box::new(Momentum {
            lookback: params[0] as usize,
            closes: Vec::new(),
        }))
    }
}

/// Serves [`Momentum`] over the protocol, failing on bar `fail_at` if given. Returns its address.
fn serve(fail_at: Option<usize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            thread::spawn(move || answer(stream, fail_at));
        }
    });
    address
}

fn answer(stream: TcpStream, fail_at: Option<usize>) {
    let mut writer = stream.try_clone().unwrap();
    let mut momentum = Momentum::default();
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        let request: Value = serde_json::from_str(&line).unwrap();
        let reply = match request["type"].as_str().unwrap() {
            "describe" => json!({
                "name": "Momentum (external)",
                "description": "Long above the close a few bars back, short below.",
                "params": [{"name": "Lookback", "default": 3, "min": 1, "max": 50, "integer": true}],
            }),
            "start" => {
                momentum.lookback = request["params"][0].as_f64().unwrap() as usize;
                json!({})
            }
            "bar" if fail_at == Some(momentum.closes.len()) => json!({"error": "boom"}),
            "bar" => {
                let bar = Candle {
                    ts: request["ts"].as_u64().unwrap(),
                    open: request["open"].as_f64().unwrap(),
                    high: request["high"].as_f64().unwrap(),
                    low: request["low"].as_f64().unwrap(),
                    close: request["close"].as_f64().unwrap(),
                    volume: request["volume"].as_f64().unwrap(),
                };
                json!({"target": momentum.on_bar(&bar).unwrap()})
            }
            "end" => json!({"notes": [{"text": format!("saw {} bars", momentum.closes.len())}]}),
            other => panic!("unexpected {other}"),
        };
        writeln!(writer, "{reply}").unwrap();
    }
}

fn plugins_at(address: &str, name: &str) -> (Plugins, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("persephone-external-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.external")),
        format!("# test\ntcp://{address}\n"),
    )
    .unwrap();
    (Host::new().unwrap().load_dir(&dir), dir)
}

fn candles(n: usize) -> Vec<Candle> {
    (0..n)
        .map(|i| {
            let p = 50_000.0 + 2_000.0 * (i as f64 / 30.0).sin() + 400.0 * (i as f64 / 5.0).cos();
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

fn backtest(
    spec: StrategySpec,
    plugin: Arc<dyn SignalFactory>,
) -> Result<BacktestReport, EngineError> {
    run_backtest(&BacktestRequest {
        instrument: InstrumentSpec {
            symbol: "BTCUSDT".into(),
            venue: "BINANCE".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 5,
        },
        bars: Timeframe::H1.into(),
        candles: candles(600),
        strategy: spec,
        plugin: Some(plugin),
        ticks: None,
        starting_balance: 100_000.0,
        fees: FeeRates::ZERO,
    })
}

#[test]
fn an_external_strategy_backtests_like_the_same_rule_in_process() {
    let (plugins, dir) = plugins_at(&serve(None), "momentum");
    assert!(plugins.errors.is_empty(), "{:?}", plugins.errors);
    let plugin = plugins.strategies.get("momentum").expect("loaded").clone();
    assert_eq!(plugin.info.name, "Momentum (external)");
    let names: Vec<&str> = plugin.info.params.iter().map(|p| p.name.as_ref()).collect();
    assert_eq!(names, ["Lookback", "Size %"]);
    assert!(
        plugin
            .address()
            .is_some_and(|a| a.to_string().starts_with("tcp://127.0.0.1:"))
    );

    let spec = StrategySpec::from_info(
        StrategyKind::Plugin("momentum".into()),
        Arc::clone(&plugin.info),
        &[5.0],
    );
    let external = backtest(spec.clone(), plugin).unwrap();
    let in_process = backtest(spec, Arc::new(InProcess)).unwrap();
    assert!(external.fills.len() > 10);
    assert_eq!(external.fills, in_process.fills);
    assert_eq!(external.equity, in_process.equity);
    assert_eq!(external.notes.len(), 1);
    assert_eq!(external.notes[0].text, "saw 600 bars");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_external_failure_stops_the_backtest_with_its_message() {
    let (plugins, dir) = plugins_at(&serve(Some(40)), "flaky");
    let plugin = plugins.strategies.get("flaky").expect("loaded").clone();
    let spec = StrategySpec::from_info(
        StrategyKind::Plugin("flaky".into()),
        Arc::clone(&plugin.info),
        &[],
    );
    let error = backtest(spec, plugin).unwrap_err();
    assert!(
        error.to_string().contains("the strategy failed: boom"),
        "{error}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_unreachable_strategy_says_how_to_retry() {
    // Bind and drop, so the port is (almost certainly) closed.
    let address = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let (plugins, dir) = plugins_at(&address, "gone");
    assert!(plugins.strategies.is_empty());
    let (file, error) = &plugins.errors[0];
    assert_eq!(file, "gone.external");
    assert!(
        error.contains("couldn't reach") && error.contains("save this file"),
        "{error}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
