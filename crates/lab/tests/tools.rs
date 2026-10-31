//! Lab through its tools, on a market imported from a CSV file, so nothing needs the network or calls
//! a language model.

use std::{
    fs,
    io::{BufRead as _, BufReader, Write as _},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use persephone_core::{
    Candle, Timeframe,
    strategy::{BuiltInStrategy, StrategySpec},
};
use persephone_lab::{
    Lab, Paths,
    cache::BarSource,
    http,
    runs::build_request,
    tools::{reference, tools},
};
use persephone_ml::models::logistic_regression;
use persephone_research::{
    market::MarketBars,
    robustness::{closed_trades, monte_carlo},
    settings::Settings,
};
use serde_json::{Value, json};

const SECRET: &str = "sk-ant-planted-secret-value";
const HOUR: u64 = 3_600_000_000_000;
/// 2024-01-01.
const START: u64 = 1_704_067_200_000_000_000;

fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "persephone-lab-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 2,000 hourly bars, as a CSV file's text.
fn csv() -> String {
    let mut text = String::from("time,open,high,low,close,volume\n");
    for i in 0..2_000_u64 {
        let mid = 50_000.0 + 1_500.0 * (i as f64 / 30.0).sin() + 200.0 * (i as f64 / 7.0).cos();
        let at = START + i * HOUR;
        let (days, minutes) = (at / (24 * HOUR), (at % (24 * HOUR)) / (HOUR / 60));
        let (year, month, day) = persephone_core::time::civil_from_days(days as i64);
        text += &format!(
            "{year:04}-{month:02}-{day:02} {:02}:{:02}:00,{:.2},{:.2},{:.2},{:.2},{:.5}\n",
            minutes / 60,
            minutes % 60,
            mid - 5.0,
            mid + 20.0,
            mid - 20.0,
            mid + 5.0,
            10.12345 + (i % 7) as f64,
        );
    }
    text
}

/// A Lab of its own, with the market in its store and things it mustn't give away beside it.
fn lab() -> (Arc<Lab>, PathBuf) {
    let dir = temp_dir();
    let lab = Lab::new(Paths::under(&dir)).unwrap();
    fs::write(
        dir.join("ai.json"),
        format!("{{\"anthropic_api_key\": \"{SECRET}\"}}"),
    )
    .unwrap();
    let file = dir.join("bars.csv");
    fs::write(&file, csv()).unwrap();
    let answer = lab
        .dispatch(
            "import_data",
            &json!({"files": [file], "symbol": "BTCUSDT", "venue": "TEST", "wait_seconds": 60}),
        )
        .unwrap();
    assert_eq!(answer["imported"], 2_000, "{answer}");
    (lab, dir)
}

fn market() -> Value {
    json!({"symbol": "BTCUSDT", "venue": "TEST", "bars": "1h"})
}

/// `market()` with more on it.
fn with(more: Value) -> Value {
    let mut args = market();
    for (key, value) in more.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}

fn ema() -> Value {
    json!({"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 20}})
}

fn tree() -> Value {
    json!({"logic": {
        "params": {"fast": 5, "slow": 20},
        "long": {"crosses_above": [{"ema": {"length": "$fast"}}, {"ema": {"length": "$slow"}}]},
        "short": {"crosses_below": [{"ema": {"length": "$fast"}}, {"ema": {"length": "$slow"}}]},
    }})
}

fn backtest(lab: &Arc<Lab>, strategy: &Value) -> Value {
    lab.dispatch(
        "run_backtest",
        &with(json!({"strategy": strategy, "wait_seconds": 120})),
    )
    .unwrap()
}

/// Every tool that works without the network, with arguments it should accept.
fn calls(lab: &Arc<Lab>, dir: &std::path::Path) -> Vec<(&'static str, Value)> {
    let run = backtest(lab, &ema());
    let run_id = run["run_id"].clone();
    let job = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": ema(), "wait_seconds": 120})),
        )
        .unwrap();
    fs::write(
        lab.paths.models.join("trend.onnx"),
        logistic_regression(&[0.5, -0.5], 0.1),
    )
    .unwrap();
    fs::write(
        dir.join("layout.ron"),
        "(workspace: 1, tiles: [(id: 7, symbol: \"BTCUSDT\", backtest: Some((invert: true)))])",
    )
    .unwrap();
    fs::write(dir.join("drawings.ron"), "{\"BTCUSDT\": [Line((a: 1.5))]}").unwrap();
    vec![
        ("get_guide", json!({"topic": "units"})),
        ("get_settings", json!({})),
        ("list_markets", json!({})),
        (
            "get_instrument",
            json!({"symbol": "BTCUSDT", "venue": "TEST"}),
        ),
        (
            "get_history_range",
            json!({"symbol": "BTCUSDT", "venue": "TEST"}),
        ),
        ("get_bars", with(json!({"from": "2024-01-02", "limit": 5}))),
        (
            "export_data",
            with(json!({"from": "2024-01-02", "to": "2024-01-03", "wait_seconds": 60})),
        ),
        ("list_indicators", json!({})),
        (
            "compute_indicator",
            with(
                json!({"indicator": "ema", "params": {"Length": 5}, "limit": 3, "wait_seconds": 60}),
            ),
        ),
        ("list_models", json!({})),
        (
            "run_model",
            json!({"model": "trend", "shape": [1, 2], "data": [0.5, 0.25]}),
        ),
        ("list_strategies", json!({})),
        ("describe_strategy", json!({"strategy": tree()})),
        ("validate_logic", json!({"logic": tree()["logic"]})),
        (
            "run_backtest",
            with(json!({"strategy": tree(), "wait_seconds": 120})),
        ),
        (
            "run_monte_carlo",
            json!({"run_id": run_id, "runs": 100, "show_run": 3, "wait_seconds": 60}),
        ),
        (
            "run_random_blocks",
            with(json!({"strategy": ema(), "blocks": 4, "length": "week", "wait_seconds": 120})),
        ),
        (
            "run_backtests",
            json!({
                "runs": [{"strategy": ema()}],
                "defaults": market(),
                "wait_seconds": 120,
            }),
        ),
        (
            "run_walk_forward",
            with(json!({
                "strategy": ema(),
                "variables": [{"name": "Fast", "min": 4, "max": 8, "step": 2}],
                "windows": 2,
                "wait_seconds": 120,
            })),
        ),
        (
            "run_optimize",
            with(json!({
                "strategy": ema(),
                "variables": [{"name": "Fast", "min": 4, "max": 8, "step": 2}],
                "wait_seconds": 120,
            })),
        ),
        ("get_report", json!({"run_id": run_id})),
        ("get_trades", json!({"run_id": run_id})),
        ("get_fills", json!({"run_id": run_id})),
        ("get_equity", json!({"run_id": run_id, "limit": 5})),
        ("get_returns", json!({"run_id": run_id})),
        ("get_calendar", json!({"run_id": run_id})),
        ("list_runs", json!({})),
        ("compare_runs", json!({"run_ids": [run_id]})),
        ("get_job", json!({"job_id": job["job_id"]})),
        ("get_layout", json!({})),
        ("get_chart_settings", json!({})),
        ("get_drawings", json!({})),
        (
            "save_result",
            json!({"name": "A try", "strategy": tree(), "run_id": run_id, "note": "worth a look"}),
        ),
        ("list_results", json!({})),
        ("get_call_log", json!({"limit": 5})),
    ]
}

/// Tools that need something these tests don't have: the network, a compiled plugin or a job to stop.
/// Each is exercised on its own elsewhere as far as it can be offline.
const NOT_CALLED_HERE: [&str; 5] = [
    "get_ticks",
    "import_data",
    "fetch_archive",
    "install_plugin",
    "cancel_job",
];

#[test]
fn every_tool_answers_as_it_says_and_none_gives_a_key_away() {
    let (lab, dir) = lab();
    let calls = calls(&lab, &dir);
    let all = tools();
    for tool in &all {
        let called = calls.iter().any(|(name, _)| *name == tool.name);
        assert!(
            called || NOT_CALLED_HERE.contains(&tool.name),
            "{} isn't tested",
            tool.name
        );
    }
    for (name, args) in calls {
        let answer = lab
            .dispatch(name, &args)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let tool = all.iter().find(|tool| tool.name == name).unwrap();
        for key in tool.returns {
            assert!(
                answer.get(*key).is_some(),
                "{name} has no `{key}`: {answer}"
            );
        }
        assert!(
            !answer.to_string().contains(SECRET),
            "{name} gave the key away"
        );
        // Its schema is one an agent can use.
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object", "{name}");
        for arg in &tool.args {
            assert!(
                schema["properties"][arg.name].is_object(),
                "{name}.{}",
                arg.name
            );
        }
    }
    let log = fs::read_to_string(dir.join("lab").join("calls.jsonl")).unwrap();
    assert!(log.contains("get_report"), "calls are logged");
}

#[test]
fn bad_calls_say_what_would_be_right() {
    let (lab, _dir) = lab();
    let error = |name: &str, args: Value| lab.dispatch(name, &args).unwrap_err();
    assert!(error("nope", json!({})).contains("no tool `nope`"));
    assert!(error("get_bars", json!({})).contains("`symbol` is required"));
    let unknown = error("get_bars", json!({"symbol": "X", "sybmol": "Y"}));
    assert!(
        unknown.contains("`sybmol`") && unknown.contains("symbol"),
        "{unknown}"
    );
    assert!(error("get_bars", json!({"symbol": 5})).contains("should be string"));
    assert!(error("get_bars", with(json!({"from": "soon"}))).contains("`from`"));
    assert!(error("get_bars", with(json!({"bars": "9x"}))).contains("`bars`"));
    assert!(error("get_report", json!({"run_id": "../../secrets"})).contains("isn't an id"));
    assert!(error("get_job", json!({"job_id": "../x"})).contains("isn't an id"));
    assert!(error("get_report", json!({"run_id": "run-1"})).contains("no run"));
    let strategy = error(
        "run_backtest",
        with(json!({"strategy": {"builtin": "nope"}, "wait_seconds": 60})),
    );
    assert!(strategy.contains("ema-cross"), "{strategy}");
    let settings = error(
        "run_backtest",
        with(json!({"strategy": ema(), "settings": {"capital": "lots"}, "wait_seconds": 60})),
    );
    assert!(settings.contains("`settings`"), "{settings}");
    let logic = lab
        .dispatch(
            "validate_logic",
            &json!({"logic": {"long": {"gtt": [1, 2]}}}),
        )
        .unwrap();
    assert_eq!(logic["valid"], false);
    assert!(logic["error"].as_str().unwrap().contains("gtt"));
    let guide = lab.dispatch("validate_logic", &json!({})).unwrap();
    assert_eq!(guide["valid"], false);
    assert!(guide["error"].is_null(), "only asking what's allowed");
    assert!(guide["allowed"]["indicators"].as_array().unwrap().len() >= 8);
}

fn store_candles(lab: &Lab) -> Vec<Candle> {
    lab.store
        .market("TEST", "BTCUSDT")
        .unwrap()
        .candles_between(Timeframe::H1.into(), 0, u64::MAX)
        .unwrap()
}

#[test]
fn a_run_read_back_gives_the_same_numbers_as_the_engine() {
    let (lab, _dir) = lab();
    let run = backtest(&lab, &ema());
    let run_id = run["run_id"].as_str().unwrap();

    // The same backtest, straight through the engine.
    let market = persephone_research::market::load_local(
        &lab.store,
        "TEST",
        "BTCUSDT",
        Timeframe::H1.into(),
        None,
    )
    .unwrap();
    let spec = StrategySpec::with_params(BuiltInStrategy::EmaCross, &[5.0, 20.0, 10.0]);
    let request = build_request(market, spec, None, &Settings::default(), 0, None).unwrap();
    let direct = persephone_research::robustness::backtest(request).unwrap();

    let fills = lab
        .dispatch("get_fills", &json!({"run_id": run_id, "limit": 50_000}))
        .unwrap();
    assert_eq!(fills["total"], direct.fills.len());
    assert!(direct.fills.len() > 10, "it trades");
    for (row, fill) in fills["rows"].as_array().unwrap().iter().zip(&direct.fills) {
        assert_eq!(row[0], fill.ts);
        assert_eq!(row[3].as_f64().unwrap(), fill.price);
        assert_eq!(row[4].as_f64().unwrap(), fill.qty);
    }
    let last = *direct.equity.last().unwrap();
    assert_eq!(run["summary"]["final_equity"].as_f64().unwrap(), last);

    // Monte Carlo on the saved run gives what it gives on the engine's own report.
    let expected = monte_carlo(
        &closed_trades(&direct),
        direct.starting_balance,
        300,
        0.5,
        9,
    )
    .unwrap();
    let drawn = lab
        .dispatch(
            "run_monte_carlo",
            &json!({"run_id": run_id, "runs": 300, "seed": 9, "wait_seconds": 60}),
        )
        .unwrap();
    assert_eq!(drawn["median"].as_f64().unwrap(), expected.median);
    assert_eq!(drawn["low"].as_f64().unwrap(), expected.low);
    assert_eq!(drawn["ruin"].as_f64().unwrap(), expected.ruin);

    // And the backtest's own place among the draws is there to see.
    assert_eq!(drawn["actual"]["backtest"], true);
    assert_eq!(
        drawn["actual"]["return"].as_f64().unwrap(),
        expected.actual_stats.return_
    );

    // The calendar adds up to the whole run.
    let months = lab
        .dispatch("get_calendar", &json!({"run_id": run_id}))
        .unwrap();
    let made: f64 = months["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["profit"].as_f64().unwrap())
        .sum();
    assert!((made - (last - direct.starting_balance)).abs() < 1e-6);

    // A logic tree matching it makes the same trades.
    let logic = backtest(&lab, &tree());
    let logic_fills = lab
        .dispatch(
            "get_fills",
            &json!({"run_id": logic["run_id"], "limit": 50_000}),
        )
        .unwrap();
    assert_eq!(logic_fills["rows"], fills["rows"]);
}

#[test]
fn a_request_is_built_as_the_app_builds_it() {
    let candles: Vec<Candle> = (0..1_500_u64)
        .map(|i| {
            let p = 50_000.0 + 1_500.0 * (i as f64 / 45.0).sin();
            Candle {
                ts: i * HOUR,
                open: p - 5.0,
                high: p + 25.0,
                low: p - 25.0,
                close: p + 5.0,
                volume: 3.0,
            }
        })
        .collect();
    let mut app = persephone::Persephone::preloaded(
        persephone_chart::ChartData::new("BTCUSDT", "BINANCE", Timeframe::H1, 2, candles.clone()),
        iced::Size::new(1400.0, 900.0),
    );
    let _ = app.update(persephone::Message::Chart(
        0,
        persephone_chart::ChartMessage::RightClicked {
            at: iced::Point::new(200.0, 200.0),
        },
    ));
    let _ = app.update(persephone::Message::Menu(
        persephone::menu::MenuMessage::Open(persephone::menu::Page::Strategies),
    ));
    let _ = app.update(persephone::Message::Menu(
        persephone::menu::MenuMessage::SetStrategy(BuiltInStrategy::EmaCross),
    ));
    let (_, theirs) = app.backtest_request(0).unwrap();
    let market = MarketBars {
        symbol: "BTCUSDT".into(),
        venue: "BINANCE".into(),
        bars: Timeframe::H1.into(),
        price_precision: 2,
        candles,
        instrument: theirs.instrument.clone(),
        fees: theirs.fees,
        ticks: None,
        fills: None,
    };
    let ours = build_request(
        market,
        theirs.strategy.clone(),
        None,
        &Settings::default(),
        theirs.trade_from,
        None,
    )
    .unwrap();
    assert_eq!(ours.candles, theirs.candles);
    assert_eq!(ours.fees, theirs.fees);
    assert_eq!(ours.sim, theirs.sim);
    assert_eq!(ours.starting_balance, theirs.starting_balance);
    assert_eq!(ours.trade_from, theirs.trade_from);
    let a = persephone_research::robustness::backtest(ours).unwrap();
    let b = persephone_research::robustness::backtest(theirs).unwrap();
    assert_eq!(a.fills, b.fills);
    assert_eq!(a.equity, b.equity);
}

#[test]
fn settings_reach_the_backtest() {
    let (lab, _dir) = lab();
    let plain = backtest(&lab, &ema());
    let inverted = lab
        .dispatch(
            "run_backtest",
            &with(json!({
                "strategy": ema(),
                "settings": {"invert": true, "capital": 50_000.0},
                "wait_seconds": 120,
            })),
        )
        .unwrap();
    let sides = |run: &Value| -> Vec<Value> {
        let fills = lab
            .dispatch(
                "get_fills",
                &json!({"run_id": run["run_id"], "limit": 50_000}),
            )
            .unwrap();
        fills["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row[2].clone())
            .collect()
    };
    let (a, b) = (sides(&plain), sides(&inverted));
    assert_eq!(a.len(), b.len());
    assert!(
        a.iter().zip(&b).all(|(a, b)| a != b),
        "the other side of every trade"
    );
    let report = lab
        .dispatch("get_report", &json!({"run_id": inverted["run_id"]}))
        .unwrap();
    assert_eq!(report["starting_balance"], 50_000.0);
}

#[test]
fn a_span_trades_only_inside_it_and_warms_up_before_it() {
    let (lab, _dir) = lab();
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({
                "strategy": ema(),
                "from": "2024-02-01",
                "to": "2024-02-15",
                "wait_seconds": 120,
            })),
        )
        .unwrap();
    let from = START + 31 * 24 * HOUR;
    let fills = lab
        .dispatch("get_fills", &json!({"run_id": run["run_id"]}))
        .unwrap();
    let rows = fills["rows"].as_array().unwrap();
    assert!(!rows.is_empty(), "it trades");
    assert!(rows.iter().all(|row| row[0].as_u64().unwrap() >= from));
    let equity = lab
        .dispatch(
            "get_equity",
            &json!({"run_id": run["run_id"], "limit": 50_000}),
        )
        .unwrap();
    assert_eq!(
        equity["rows"][0][1], "2024-02-01 00:00",
        "from the span's first bar"
    );
}

#[test]
fn pages_join_up_to_the_same_data() {
    let (lab, _dir) = lab();
    let mut paged = Vec::new();
    let mut cursor = None::<String>;
    let mut pages = 0;
    loop {
        let mut args = with(json!({"from": "2024-01-01", "limit": 700}));
        if let Some(cursor) = &cursor {
            args["cursor"] = json!(cursor);
        }
        let answer = lab.dispatch("get_bars", &args).unwrap();
        paged.extend(answer["rows"].as_array().unwrap().clone());
        pages += 1;
        match answer["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(paged.len(), 2_000);
    let whole: Vec<u64> = store_candles(&lab).iter().map(|c| c.ts).collect();
    let got: Vec<u64> = paged.iter().map(|row| row[0].as_u64().unwrap()).collect();
    assert_eq!(got, whole);

    // Latest bars when no start is given.
    let latest = lab
        .dispatch("get_bars", &with(json!({"limit": 3})))
        .unwrap();
    assert_eq!(latest["rows"].as_array().unwrap().len(), 3);
    assert_eq!(latest["rows"][2][0], json!(whole[1_999]));

    // A run's pages too.
    let run = backtest(&lab, &ema());
    let first = lab
        .dispatch(
            "get_equity",
            &json!({"run_id": run["run_id"], "limit": 1_500}),
        )
        .unwrap();
    let rest = lab
        .dispatch(
            "get_equity",
            &json!({"run_id": run["run_id"], "limit": 1_500, "cursor": first["next_cursor"]}),
        )
        .unwrap();
    assert_eq!(first["total"], 2_000);
    assert_eq!(rest["rows"].as_array().unwrap().len(), 500);
    assert!(rest["next_cursor"].is_null());
}

#[test]
fn exports_write_every_bar_and_never_overwrite() {
    let (lab, dir) = lab();
    let csv_path = dir.join("out.csv");
    let parquet_path = dir.join("out.parquet");
    let answer = lab
        .dispatch(
            "export_data",
            &with(json!({"from": "2024-01-01", "path": csv_path, "wait_seconds": 60})),
        )
        .unwrap();
    assert_eq!(answer["rows"], 2_000);
    let text = fs::read_to_string(&csv_path).unwrap();
    assert!(
        text.starts_with("ts,open,high,low,close,volume\n"),
        "{}",
        &text[..40]
    );
    assert_eq!(text.lines().count(), 2_001);

    let parquet = lab
        .dispatch(
            "export_data",
            &with(json!({"from": "2024-01-01", "format": "parquet", "path": parquet_path, "wait_seconds": 60})),
        )
        .unwrap();
    assert_eq!(parquet["rows"], 2_000);
    assert!(fs::read(&parquet_path).unwrap().starts_with(b"PAR1"));

    let again = lab.dispatch(
        "export_data",
        &with(json!({"from": "2024-01-01", "path": csv_path, "wait_seconds": 60})),
    );
    assert!(again.unwrap_err().contains("won't overwrite"));
}

#[test]
fn slow_work_can_be_checked_on_and_cancelled() {
    let (lab, _dir) = lab();
    let started = lab
        .dispatch(
            "run_random_blocks",
            &with(json!({"strategy": ema(), "blocks": 400, "length": "month", "wait_seconds": 0})),
        )
        .unwrap();
    assert!(started["status"] == "running", "{started}");
    let id = started["job_id"].clone();
    let cancelled = lab.dispatch("cancel_job", &json!({"job_id": id})).unwrap();
    assert_ne!(cancelled["status"], "done", "{cancelled}");

    // It stops short of all 400, saying so.
    let deadline = Instant::now() + Duration::from_secs(60);
    let error = loop {
        match lab.dispatch("get_job", &json!({"job_id": id})) {
            Err(error) => break error,
            Ok(status) => assert_ne!(status["status"], "done", "it should've stopped: {status}"),
        }
        assert!(Instant::now() < deadline, "it never stopped");
        thread::sleep(Duration::from_millis(20));
    };
    assert!(error.contains("Stopped"), "{error}");

    // One left alone finishes, reports how far it got and keeps its answer.
    let run = lab
        .dispatch(
            "run_random_blocks",
            &with(json!({"strategy": ema(), "blocks": 12, "length": "week", "wait_seconds": 0})),
        )
        .unwrap();
    let id = run["job_id"].clone();
    let deadline = Instant::now() + Duration::from_secs(60);
    let done = loop {
        let status = lab.dispatch("get_job", &json!({"job_id": id})).unwrap();
        if status["status"] == "done" {
            break status;
        }
        // Told once the blocks are picked.
        assert!(status["total"] == 0 || status["total"] == 12, "{status}");
        assert!(Instant::now() < deadline, "it never finished");
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(done["result"]["blocks"].as_array().unwrap().len(), 12);
}

#[test]
fn runs_and_jobs_outlast_the_process() {
    let (lab, dir) = lab();
    let run = backtest(&lab, &ema());
    let job = run["job_id"].clone();
    let again = Lab::new(Paths::under(&dir)).unwrap();
    let report = again
        .dispatch("get_report", &json!({"run_id": run["run_id"]}))
        .unwrap();
    assert_eq!(report["summary"], run["summary"]);
    let list = again.dispatch("list_runs", &json!({})).unwrap();
    assert_eq!(list["rows"][0]["run_id"], run["run_id"]);
    let saved = again.dispatch("get_job", &json!({"job_id": job})).unwrap();
    assert_eq!(saved["status"], "done");
    assert_eq!(saved["result"]["run_id"], run["run_id"]);
}

#[test]
fn the_apps_saved_files_read_as_json() {
    let (lab, dir) = lab();
    fs::write(
        dir.join("layout.ron"),
        r#"(
    layout: (root: Leaf(7), focused: 7, fullscreen: false, next_id: 8),
    tiles: [(
        id: 7,
        symbol: "BNBUSDT",
        timeframe: "15m",
        indicators: [],
        strategy: Some((kind: "rsi-reversion", params: [16.0, 30.0])),
        span: Some((1625234400000000000, 1648132200000000000)),
        backtest: Some((
            stock: true,
            commission: (on: true, value: r#per-order(1.0)),
            precision: standard,
        )),
    )],
    workspace: 1,
)"#,
    )
    .unwrap();
    let layout = lab.dispatch("get_layout", &json!({})).unwrap();
    assert_eq!(layout["layout"]["workspace"], 1);
    assert_eq!(layout["layout"]["tiles"][0]["symbol"], "BNBUSDT");
    let charts = lab.dispatch("get_chart_settings", &json!({})).unwrap();
    assert_eq!(charts["charts"][0]["strategy"]["kind"], "rsi-reversion");
    assert_eq!(
        charts["charts"][0]["span"][0],
        1_625_234_400_000_000_000_u64
    );
    // Which choice each one is comes through, not just its number.
    let settings = &charts["charts"][0]["backtest"];
    assert_eq!(settings["commission"]["value"]["per-order"], 1.0);
    assert_eq!(settings["precision"], "standard");
    assert_eq!(layout["layout"]["layout"]["root"]["Leaf"], 7);
    assert_eq!(layout["layout"]["layout"]["fullscreen"], false);
}

#[test]
fn saved_results_are_there_for_the_app() {
    let (lab, _dir) = lab();
    let run = backtest(&lab, &tree());
    let saved = lab
        .dispatch(
            "save_result",
            &json!({
                "name": "Fast over slow",
                "strategy": tree(),
                "run_id": run["run_id"],
                "settings": {"invert": true},
            }),
        )
        .unwrap();
    let list = lab.dispatch("list_results", &json!({})).unwrap();
    let first = &list["results"][0];
    assert_eq!(first["id"], saved["id"]);
    assert_eq!(first["name"], "Fast over slow");
    assert_eq!(first["summary"], run["summary"]);
    assert_eq!(first["settings"]["invert"], true);
    let bad = lab.dispatch(
        "save_result",
        &json!({"name": "x", "strategy": {"logic": {}}}),
    );
    assert!(bad.unwrap_err().contains("long"));
}

#[test]
fn prune_removes_old_runs_only_and_keeps_everything_else() {
    let (lab, dir) = lab();
    let old = backtest(&lab, &ema());
    let saved = lab
        .dispatch("save_result", &json!({"name": "Keep", "strategy": ema()}))
        .unwrap();
    // Nothing is old yet, so nothing goes.
    assert_eq!(lab.prune(Duration::from_secs(3_600)).unwrap().0, 0);
    let runs = dir.join("lab").join("runs");
    let earlier = std::time::SystemTime::now() - Duration::from_secs(7_200);
    for entry in fs::read_dir(&runs).unwrap().flatten() {
        fs::File::options()
            .write(true)
            .open(entry.path())
            .unwrap()
            .set_modified(earlier)
            .unwrap();
    }
    let new = backtest(&lab, &tree());
    let (files, bytes) = lab.prune(Duration::from_secs(3_600)).unwrap();
    assert_eq!(files, 2, "the old run and its summary");
    assert!(bytes > 0);
    assert!(
        lab.dispatch("get_report", &json!({"run_id": old["run_id"]}))
            .is_err()
    );
    assert!(
        lab.dispatch("get_report", &json!({"run_id": new["run_id"]}))
            .is_ok()
    );
    let list = lab.dispatch("list_results", &json!({})).unwrap();
    assert_eq!(list["results"][0]["id"], saved["id"]);
    assert_eq!(store_candles(&lab).len(), 2_000, "the market's still there");
}

#[test]
fn a_run_saved_before_they_were_packed_still_reads() {
    let (lab, dir) = lab();
    let run = backtest(&lab, &ema());
    let id = run["run_id"].as_str().unwrap();
    let report = lab.dispatch("get_report", &json!({"run_id": id})).unwrap();
    let runs = dir.join("lab").join("runs");
    let packed = fs::read(runs.join(format!("{id}.json.gz"))).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&packed[..]), &mut text)
        .unwrap();
    fs::remove_file(runs.join(format!("{id}.json.gz"))).unwrap();
    fs::write(runs.join(format!("{id}.json")), &text).unwrap();
    let fresh = Lab::new(Paths::under(&dir)).unwrap();
    assert_eq!(
        fresh
            .dispatch("get_report", &json!({"run_id": id}))
            .unwrap(),
        report
    );
    assert!(
        packed.len() < text.len() / 2,
        "{} against {}",
        packed.len(),
        text.len()
    );
}

/// A market from "Binance" with no network: `HOURS` of hourly bars from `START`, counting requests.
struct FakeBinance {
    requests: AtomicUsize,
}

const HOURS: u64 = 3_000;

impl BarSource for FakeBinance {
    fn instrument(&self, symbol: &str) -> Result<persephone_core::InstrumentSpec, String> {
        Ok(persephone_core::InstrumentSpec {
            symbol: symbol.into(),
            venue: "BINANCE".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 5,
        })
    }

    fn klines(
        &self,
        _symbol: &str,
        _timeframe: Timeframe,
        start: u64,
        end: u64,
    ) -> Result<Vec<Candle>, String> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        Ok((start / HOUR..end / HOUR)
            .filter(|hour| (START / HOUR..=START / HOUR + HOURS).contains(hour))
            .map(|hour| {
                let mid = 50_000.0 + 1_500.0 * (hour as f64 / 30.0).sin();
                Candle {
                    ts: hour * HOUR,
                    open: mid - 5.0,
                    high: mid + 20.0,
                    low: mid - 20.0,
                    close: mid + 5.0,
                    volume: 10.0,
                }
            })
            .collect())
    }

    fn now(&self) -> u64 {
        START + HOURS * HOUR + HOUR / 2
    }
}

fn binance_lab() -> (Arc<Lab>, Arc<FakeBinance>) {
    let source = Arc::new(FakeBinance {
        requests: AtomicUsize::new(0),
    });
    let lab = Lab::with_source(Paths::under(temp_dir()), source.clone()).unwrap();
    (lab, source)
}

#[test]
fn backtests_of_binance_markets_repeat_exactly_without_asking_again() {
    let (lab, source) = binance_lab();
    let args = json!({
        "symbol": "BTCUSDT", "bars": "1h", "strategy": ema(), "wait_seconds": 120,
    });
    let first = lab.dispatch("run_backtest", &args).unwrap();
    let asked = source.requests.load(Ordering::Relaxed);
    assert!(asked > 0);
    let second = lab.dispatch("run_backtest", &args).unwrap();
    assert_eq!(
        source.requests.load(Ordering::Relaxed),
        asked,
        "nothing more"
    );
    assert_eq!(first["summary"], second["summary"]);

    // It ends on the last closed bar, and says which that was.
    let last = START / HOUR + HOURS - 1;
    let (days, hours) = (last / 24, last % 24);
    let (year, month, day) = persephone_core::time::civil_from_days(days as i64);
    assert_eq!(
        first["summary"]["last_bar"],
        format!("{year:04}-{month:02}-{day:02} {hours:02}:00")
    );

    // Many workers on a cold market, through the tools: one download.
    let (lab, source) = binance_lab();
    let batch = lab
        .dispatch(
            "run_random_blocks",
            &json!({
                "symbol": "BTCUSDT", "bars": "1h", "strategy": ema(), "blocks": 8,
                "length": "week", "wait_seconds": 120,
            }),
        )
        .unwrap();
    assert_eq!(batch["blocks"].as_array().unwrap().len(), 8);
    assert_eq!(
        source.requests.load(Ordering::Relaxed),
        5,
        "one download, of 5,000 bars at 1,000 a request"
    );
}

#[test]
fn binance_pages_join_up_and_stop_at_the_last_closed_bar() {
    let (lab, _source) = binance_lab();
    let mut got = Vec::new();
    let mut cursor = None::<String>;
    loop {
        let mut args = json!({"symbol": "BTCUSDT", "bars": "1h", "from": START, "limit": 700});
        if let Some(cursor) = &cursor {
            args["cursor"] = json!(cursor);
        }
        let answer = lab.dispatch("get_bars", &args).unwrap();
        got.extend(
            answer["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row[0].as_u64().unwrap()),
        );
        match answer["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    let expected: Vec<u64> = (0..HOURS).map(|hour| START + hour * HOUR).collect();
    assert_eq!(got, expected);
}

#[test]
fn a_plugin_that_isnt_one_is_refused_and_nothing_is_copied() {
    let (lab, dir) = lab();
    let fake = dir.join("fake.wasm");
    fs::write(&fake, "not a plugin").unwrap();
    assert!(
        lab.dispatch("install_plugin", &json!({"path": fake}))
            .is_err()
    );
    assert!(!lab.paths.plugins.join("fake.wasm").exists());
    let text = lab.dispatch("install_plugin", &json!({"path": dir.join("bars.csv")}));
    assert!(text.unwrap_err().contains(".wasm"));
    let ticks = lab.dispatch(
        "get_ticks",
        &json!({"symbol": "NONE", "venue": "TEST", "from": "2024-01-01"}),
    );
    assert!(ticks.is_err());
}

fn call_http(
    port: u16,
    token: Option<&str>,
    host: Option<&str>,
    origin: Option<&str>,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let client = reqwest::blocking::Client::new();
    let mut request = client
        .post(format!("http://127.0.0.1:{port}{path}"))
        .body(body.to_string());
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(host) = host {
        request = request.header("Host", host);
    }
    if let Some(origin) = origin {
        request = request.header("Origin", origin);
    }
    let response = request.send().unwrap();
    let status = response.status().as_u16();
    (status, response.json().unwrap())
}

fn start_http(lab: &Arc<Lab>) -> (u16, String) {
    let (ready, port) = mpsc::channel();
    let server = Arc::clone(lab);
    thread::spawn(move || {
        http::serve(&server, 0, |port| ready.send(port).unwrap()).unwrap();
    });
    let port = port.recv_timeout(Duration::from_secs(10)).unwrap();
    (port, http::token(lab).unwrap())
}

#[test]
fn http_only_answers_with_the_token_at_its_own_address() {
    let (lab, _dir) = lab();
    let (port, token) = start_http(&lab);
    let path = "/v1/tools/list_markets";
    let empty = json!({});
    assert_eq!(call_http(port, None, None, None, path, &empty).0, 401);
    assert_eq!(
        call_http(port, Some("wrong"), None, None, path, &empty).0,
        401
    );
    assert_eq!(
        call_http(port, Some(&token), Some("evil.example"), None, path, &empty).0,
        403
    );
    assert_eq!(
        call_http(
            port,
            Some(&token),
            None,
            Some("https://evil.example"),
            path,
            &empty
        )
        .0,
        403
    );
    let (status, answer) = call_http(port, Some(&token), None, None, path, &empty);
    assert_eq!(status, 200);
    assert_eq!(answer["local"][0]["symbol"], "BTCUSDT");
    let (status, answer) = call_http(port, Some(&token), None, None, "/v1/tools/nope", &empty);
    assert_eq!(status, 400);
    assert!(answer["error"].as_str().unwrap().contains("no tool"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(lab.paths.lab().join("http-token"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "only you can read the token");
    }
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_persephone-lab")
}

fn call_cli(dir: &std::path::Path, tool: &str, args: &Value) -> Value {
    let output = Command::new(bin())
        .args(["--data"])
        .arg(dir)
        .args(["call", tool, &args.to_string()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Talks to the server the way an agent does: JSON-RPC, a line at a time.
struct McpClient {
    child: std::process::Child,
    output: BufReader<std::process::ChildStdout>,
    next: u64,
}

impl McpClient {
    fn start(dir: &std::path::Path) -> Self {
        let mut child = Command::new(bin())
            .args(["--data"])
            .arg(dir)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            output,
            next: 1,
        };
        let hello = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"},
            }),
        );
        assert_eq!(hello["serverInfo"]["name"], "persephone-lab", "{hello}");
        client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        client
    }

    fn send(&mut self, message: &Value) {
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(
                self.output.read_line(&mut line).unwrap() > 0,
                "the server hung up"
            );
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == id {
                return message["result"].clone();
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn every_way_in_gives_the_same_answer() {
    let (lab, dir) = lab();
    let args = with(json!({"from": "2024-01-02", "limit": 4}));
    let direct = lab.dispatch("get_bars", &args).unwrap();
    assert_eq!(direct["rows"].as_array().unwrap().len(), 4);

    let cli = call_cli(&dir, "get_bars", &args);
    assert_eq!(cli, direct);

    let (port, token) = start_http(&lab);
    let (status, http_answer) =
        call_http(port, Some(&token), None, None, "/v1/tools/get_bars", &args);
    assert_eq!(status, 200);
    assert_eq!(http_answer, direct);

    let mut mcp = McpClient::start(&dir);
    let listed = mcp.request("tools/list", json!({}));
    let names: Vec<&str> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let all = tools();
    assert_eq!(names.len(), all.len());
    assert!(all.iter().all(|tool| names.contains(&tool.name)));
    assert_eq!(listed["tools"][0]["inputSchema"]["type"], "object");

    let result = mcp.request("tools/call", json!({"name": "get_bars", "arguments": args}));
    assert_ne!(result["isError"], true, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), direct);

    // A failure comes back as one, not as a broken connection.
    let result = mcp.request("tools/call", json!({"name": "nope", "arguments": {}}));
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("no tool")
    );

    // A real backtest over MCP, read back by its run.
    let result = mcp.request(
        "tools/call",
        json!({
            "name": "run_backtest",
            "arguments": with(json!({"strategy": tree(), "wait_seconds": 120})),
        }),
    );
    let run: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    let report = lab
        .dispatch("get_report", &json!({"run_id": run["run_id"]}))
        .unwrap();
    assert_eq!(report["summary"], run["summary"]);
}

/// Every call in the guide: each ```json block that names a tool.
fn guide_calls() -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/lab-agents.md");
    let text = fs::read_to_string(path).unwrap();
    text.split("```json")
        .skip(1)
        .map(|block| {
            let body = block.split("```").next().unwrap();
            serde_json::from_str::<Value>(body)
                .unwrap_or_else(|e| panic!("a call in the guide isn't JSON: {e}: {body}"))
        })
        .collect()
}

#[test]
fn every_call_in_the_guide_works() {
    let (lab, _source) = binance_lab();
    let calls = guide_calls();
    assert!(calls.len() >= 9, "{}", calls.len());
    let mut run = String::new();
    let mut most_trades = 0;
    let mut job = String::new();
    for call in calls {
        let tool = call["tool"].as_str().unwrap();
        // What the guide leaves for an earlier call to fill in.
        let text = call["args"]
            .to_string()
            .replace("$RUN", &run)
            .replace("$JOB", &job);
        let mut args: Value = serde_json::from_str(&text).unwrap();
        if tools().iter().any(|t| t.name == tool && t.job) {
            args["wait_seconds"] = json!(120);
        }
        let answer = lab
            .dispatch(tool, &args)
            .unwrap_or_else(|error| panic!("{tool} from the guide: {error}"));
        if tool == "validate_logic" {
            assert_eq!(answer["valid"], true, "{answer}");
        }
        // `$RUN` is the run with the most trades so far, as Monte Carlo needs a few.
        let trades = answer["summary"]["trades"].as_u64().unwrap_or(0);
        if let Some(id) = answer["run_id"].as_str().filter(|_| trades >= most_trades) {
            run = id.to_owned();
            most_trades = trades;
        }
        if let Some(id) = answer["job_id"].as_str() {
            job = id.to_owned();
        }
    }
}

#[test]
fn the_guide_covers_every_setting_and_says_true_things() {
    let (lab, _source) = binance_lab();
    let whole = lab.dispatch("get_guide", &json!({"topic": "all"})).unwrap();
    let text = whole["guide"].as_str().unwrap();
    let defaults = lab.dispatch("get_settings", &json!({})).unwrap();
    for name in defaults["settings"].as_object().unwrap().keys() {
        assert!(
            text.contains(&format!("`{name}`")),
            "`{name}` isn't in the guide"
        );
    }

    // What it says about how backtests work, against what they do.
    let run = lab
        .dispatch(
            "run_backtest",
            &json!({
                "symbol": "BTCUSDT", "bars": "1h", "from": "2024-02-01", "to": "2024-03-01",
                "strategy": {"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 20}},
                "wait_seconds": 120,
            }),
        )
        .unwrap();
    assert!(
        run["summary"]["max_drawdown"].as_f64().unwrap() >= 0.0,
        "falls are positive"
    );
    let fills = lab
        .dispatch("get_fills", &json!({"run_id": run["run_id"]}))
        .unwrap();
    let first = &fills["rows"][0];
    // At the close of the bar that decided, with no costs set, for 10% of the capital.
    let bars = lab
        .dispatch(
            "get_bars",
            &json!({"symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "limit": 3_000}),
        )
        .unwrap();
    let fill_ts = first[0].as_u64().unwrap();
    let bar = bars["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bar| bar[0].as_u64().unwrap() + HOUR == fill_ts)
        .unwrap_or_else(|| panic!("no bar closes at the fill: {first}"));
    let close = bar[5].as_f64().unwrap();
    assert!(
        (first[3].as_f64().unwrap() - close).abs() < 0.01,
        "it fills at that bar's close: {first}"
    );
    let worth = first[3].as_f64().unwrap() * first[4].as_f64().unwrap();
    assert!((worth - 10_000.0).abs() < 100.0, "{worth}");
    // Every opening is the same size however the run has gone: reversals are the two together.
    for fill in fills["rows"].as_array().unwrap().iter().take(40) {
        let worth = fill[3].as_f64().unwrap() * fill[4].as_f64().unwrap();
        assert!(worth > 9_000.0 && worth < 21_500.0, "{fill}");
    }
    let fee = first[5].as_f64().unwrap();
    assert!((fee / worth - 0.001).abs() < 1e-6, "0.1% unless set: {fee}");
}

#[test]
fn the_guide_comes_a_topic_at_a_time() {
    let (lab, _source) = binance_lab();
    let call = |args: Value| lab.dispatch("get_guide", &args);
    let brief = call(json!({})).unwrap();
    let listed = brief["topics"].as_array().unwrap();
    assert!(listed.len() >= 8, "{listed:?}");
    assert!(
        brief["guide"].as_str().unwrap().len() < 800,
        "only the opening"
    );
    let whole = call(json!({"topic": "all"})).unwrap()["guide"]
        .as_str()
        .unwrap()
        .len();
    for entry in listed {
        let topic = entry["topic"].as_str().unwrap();
        let about = entry["about"].as_str().unwrap();
        assert!(about.ends_with('.'), "{topic}: {about}");
        let text = call(json!({"topic": topic})).unwrap()["guide"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            text.len() > 100 && text.len() < whole / 2,
            "{topic}: {}",
            text.len()
        );
    }
    let error = call(json!({"topic": "nope"})).unwrap_err();
    assert!(
        error.contains("settings") && error.contains("all"),
        "{error}"
    );
    // The settings it describes are ones that can be given.
    let defaults = lab.dispatch("get_settings", &json!({})).unwrap();
    let settings: Settings = serde_json::from_value(defaults["settings"].clone()).unwrap();
    assert_eq!(settings, Settings::default());
}

#[test]
fn answers_with_no_limit_stay_small_and_paging_reaches_every_row() {
    let (lab, _dir) = lab();
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": ema(), "wait_seconds": 120})),
        )
        .unwrap();
    let id = run["run_id"].clone();
    for (tool, args) in [
        ("get_bars", with(json!({"from": "2024-01-01"}))),
        ("get_equity", json!({"run_id": id})),
        ("get_returns", json!({"run_id": id})),
        ("get_fills", json!({"run_id": id})),
        ("get_trades", json!({"run_id": id})),
        (
            "compute_indicator",
            with(json!({
                "indicator": "sma", "from": "2024-01-01", "wait_seconds": 60,
            })),
        ),
    ] {
        let answer = lab.dispatch(tool, &args).unwrap();
        let rows = answer["rows"].as_array().unwrap().len();
        assert!(rows <= 100, "{tool}: {rows}");
        let bytes = answer.to_string().len();
        assert!(bytes < 40_000, "{tool}: {bytes} bytes");
        // Every page together is what one huge page gives.
        let mut all = lab_rows(&lab, tool, &args);
        let mut whole = args.clone();
        whole["limit"] = json!(50_000);
        assert_eq!(
            std::mem::take(&mut all),
            lab.dispatch(tool, &whole).unwrap()["rows"]
                .as_array()
                .unwrap()
                .clone(),
            "{tool}"
        );
    }
}

/// All of a paged tool's rows, walking its pages.
fn lab_rows(lab: &Arc<Lab>, tool: &str, args: &Value) -> Vec<Value> {
    let mut args = args.clone();
    let mut rows = Vec::new();
    loop {
        let answer = lab.dispatch(tool, &args).unwrap();
        rows.extend(answer["rows"].as_array().unwrap().iter().cloned());
        match answer["next_cursor"].as_str() {
            Some(cursor) => args["cursor"] = json!(cursor),
            None => return rows,
        }
    }
}

#[test]
fn optimizing_lists_the_best_values_and_says_how_many_there_were() {
    let (lab, _dir) = lab();
    let answer = lab
        .dispatch(
            "run_optimize",
            &with(json!({
                "strategy": ema(), "limit": 3, "wait_seconds": 120,
                "variables": [{"name": "Fast", "min": 2, "max": 9, "step": 1}],
            })),
        )
        .unwrap();
    let values = answer["values"].as_array().unwrap();
    assert_eq!(values.len(), 3);
    assert_eq!(answer["total"], 8);
    let scores: Vec<f64> = values
        .iter()
        .map(|v| v["score"].as_f64().unwrap())
        .collect();
    assert!(
        scores.windows(2).all(|pair| pair[0] >= pair[1]),
        "{scores:?}"
    );
}

#[test]
fn many_backtests_in_one_call_match_the_same_ones_alone() {
    let (lab, _dir) = lab();
    let strategy =
        |fast: u32| json!({"builtin": "ema-cross", "params": {"Fast": fast, "Slow": 40}});
    let answer = lab
        .dispatch(
            "run_backtests",
            &json!({
                "defaults": market(),
                "runs": [
                    {"strategy": strategy(5)},
                    {"strategy": {"builtin": "no-such-thing"}},
                    {"strategy": strategy(9)},
                    {"strategy": strategy(9), "not_an_argument": 1},
                ],
                "wait_seconds": 120,
            }),
        )
        .unwrap();
    assert_eq!(answer["total"], 4);
    assert_eq!(answer["succeeded"], 2, "{answer}");
    assert_eq!(answer["failed"], 2, "{answer}");
    let columns: Vec<&str> = answer["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    let at = |name: &str| columns.iter().position(|c| *c == name).unwrap();
    let rows = answer["rows"].as_array().unwrap();
    for row in &rows[..2] {
        let fast = if row[at("index")] == 0 { 5 } else { 9 };
        let alone = lab
            .dispatch(
                "run_backtest",
                &with(json!({"strategy": strategy(fast), "wait_seconds": 120})),
            )
            .unwrap();
        assert_eq!(row[at("net_return")], alone["summary"]["net_return"]);
        assert_eq!(row[at("trades")], alone["summary"]["trades"]);
        // It's saved, so everything about a run works on it.
        lab.dispatch("get_report", &json!({"run_id": row[at("run_id")]}))
            .unwrap();
    }
    assert!(
        rows[0][at("net_return")].as_f64() >= rows[1][at("net_return")].as_f64(),
        "best first"
    );
    assert!(
        rows[2][at("error")].is_string() && rows[3][at("error")].is_string(),
        "{rows:?}"
    );

    // The log shows each as what it was.
    let log = lab
        .dispatch(
            "get_call_log",
            &json!({"tool": "run_backtest", "limit": 50}),
        )
        .unwrap();
    assert!(log["rows"].as_array().unwrap().len() >= 4, "{log}");
}

#[test]
fn a_batch_reports_progress_can_be_waited_on_and_can_be_cancelled() {
    let (lab, _dir) = lab();
    let run = json!({"strategy": ema()});
    let start = |count: usize| {
        lab.dispatch(
            "run_backtests",
            &json!({"defaults": market(), "runs": vec![run.clone(); count], "wait_seconds": 0}),
        )
        .unwrap()
    };
    let job = start(16);
    assert_eq!(job["status"], "running", "{job}");
    let id = job["job_id"].clone();
    // A long wait comes back as soon as it's done, not at the deadline.
    let began = Instant::now();
    let done = lab
        .dispatch("get_job", &json!({"job_id": id, "wait_seconds": 600}))
        .unwrap();
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(done["result"]["succeeded"], 16);
    assert!(began.elapsed() < Duration::from_secs(300));

    let job = start(2_000);
    let id = job["job_id"].clone();
    // A short wait comes back at its deadline.
    let began = Instant::now();
    let running = lab
        .dispatch("get_job", &json!({"job_id": id, "wait_seconds": 0.05}))
        .unwrap();
    assert_eq!(running["status"], "running", "{running}");
    assert!(began.elapsed() >= Duration::from_millis(50));
    assert_eq!(running["total"], 2_000);

    lab.dispatch("cancel_job", &json!({"job_id": id})).unwrap();
    let ended = lab
        .dispatch("get_job", &json!({"job_id": id, "wait_seconds": 600}))
        .unwrap();
    assert_eq!(ended["status"], "done", "{ended}");
    let result = &ended["result"];
    assert!(result["not_run"].as_u64().unwrap() > 0, "{result}");
    assert_eq!(result["total"], 2_000);
}

#[test]
fn pages_of_ticks_miss_none_even_when_they_share_a_time() {
    let (lab, dir) = lab();
    // Three ticks each second, over three days, so pages end between ticks of one time.
    let file = dir.join("ticks.csv");
    let mut text = String::from("time,price,size\n");
    for second in 0..3_000 {
        for tick in 0..3 {
            let at = second * 86;
            text += &format!(
                "2024-03-{:02} {:02}:{:02}:{:02},{}.5,1\n",
                1 + at / 86_400,
                at / 3_600 % 24,
                at / 60 % 60,
                at % 60,
                second * 3 + tick,
            );
        }
    }
    fs::write(&file, text).unwrap();
    lab.dispatch(
        "import_data",
        &json!({"files": [file], "symbol": "ETHUSDT", "venue": "TEST", "wait_seconds": 60}),
    )
    .unwrap();
    let args = json!({"symbol": "ETHUSDT", "venue": "TEST", "from": "2024-01-01", "limit": 100});
    let rows = lab_rows(&lab, "get_ticks", &args);
    let prices: Vec<f64> = rows.iter().map(|row| row[1].as_f64().unwrap()).collect();
    let expected: Vec<f64> = (0..9_000).map(|i| f64::from(i) + 0.5).collect();
    assert_eq!(prices, expected);

    // Past the market's end is just the end, and no default answer is huge.
    let first = lab
        .dispatch(
            "get_ticks",
            &json!({"symbol": "ETHUSDT", "venue": "TEST", "from": "2024-01-01"}),
        )
        .unwrap();
    assert_eq!(first["rows"].as_array().unwrap().len(), 100);
    let none = lab
        .dispatch(
            "get_ticks",
            &json!({"symbol": "ETHUSDT", "venue": "TEST", "from": "2030-01-01"}),
        )
        .unwrap();
    assert!(none["rows"].as_array().unwrap().is_empty());
    assert!(none["next_cursor"].is_null());
}

#[test]
fn the_call_log_reads_back_from_its_end_a_page_at_a_time() {
    let (lab, _dir) = lab();
    let path = lab.paths.lab().join("calls.jsonl");
    // Far more than one read's worth, with lines of different lengths.
    let mut text = String::new();
    for at in 0..20_000 {
        let tool = if at % 2 == 0 { "even" } else { "odd" };
        let padding = "x".repeat(at % 300);
        text += &json!({"at": at, "tool": tool, "args": {"p": padding}, "ok": true, "error": null})
            .to_string();
        text.push('\n');
    }
    fs::write(&path, text).unwrap();

    let walk = |tool: &str| -> Vec<u64> {
        let mut args = json!({"tool": tool, "limit": 700});
        let mut seen = Vec::new();
        loop {
            let answer = lab.dispatch("get_call_log", &args).unwrap();
            seen.extend(
                answer["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["at"].as_u64().unwrap()),
            );
            match answer["next_cursor"].as_str() {
                Some(cursor) => args["cursor"] = json!(cursor),
                None => return seen,
            }
        }
    };
    let even: Vec<u64> = (0..20_000).rev().filter(|at| at % 2 == 0).collect();
    assert_eq!(walk("even"), even, "newest first, none missed or repeated");

    let newest = lab.dispatch("get_call_log", &json!({})).unwrap();
    assert_eq!(newest["rows"].as_array().unwrap().len(), 50, "the default");
}

#[test]
fn runs_list_newest_first_and_compare_without_unpacking_them() {
    let (lab, _dir) = lab();
    let ids: Vec<Value> = (0..3)
        .map(|_| {
            lab.dispatch(
                "run_backtest",
                &with(json!({"strategy": ema(), "wait_seconds": 120})),
            )
            .unwrap()["run_id"]
                .clone()
        })
        .collect();
    let list = lab.dispatch("list_runs", &json!({"limit": 2})).unwrap();
    let rows = list["rows"].as_array().unwrap();
    assert_eq!(list["total"], 3);
    assert_eq!(rows[0]["run_id"], ids[2]);
    assert_eq!(rows[1]["run_id"], ids[1]);
    assert!(rows[0].get("request").is_none(), "kept short");
    assert!(rows[0]["summary"]["trades"].is_number());
    let more = lab
        .dispatch("list_runs", &json!({"cursor": list["next_cursor"]}))
        .unwrap();
    assert_eq!(more["rows"][0]["run_id"], ids[0]);

    // The full run isn't needed, and is gone here, to show it.
    let runs = lab.paths.lab().join("runs");
    let id = ids[0].as_str().unwrap();
    fs::remove_file(runs.join(format!("{id}.json.gz"))).unwrap();
    let compared = lab
        .dispatch("compare_runs", &json!({"run_ids": [id]}))
        .unwrap();
    assert!(
        compared["runs"][0]["request"]["strategy"].is_object(),
        "{compared}"
    );

    // One saved before summaries held the request reads the full run.
    let id = ids[1].as_str().unwrap();
    fs::write(
        runs.join(format!("{id}.summary")),
        json!({"run_id": id, "created": 1, "summary": {}}).to_string(),
    )
    .unwrap();
    let compared = lab
        .dispatch("compare_runs", &json!({"run_ids": [id]}))
        .unwrap();
    assert!(
        compared["runs"][0]["request"]["strategy"].is_object(),
        "{compared}"
    );
}

const JANUARY_2024: u64 = 19_723 * 24 * HOUR;

/// A market of `closes`, one bar an hour from 2024-01-01, that doesn't move within a bar.
fn market_of(closes: &[f64]) -> (Arc<Lab>, Vec<u64>) {
    let (lab, dir) = lab();
    let file = dir.join("hand.csv");
    let mut text = String::from("time,open,high,low,close,volume\n");
    for (i, close) in closes.iter().enumerate() {
        text += &format!(
            "2024-01-{:02} {:02}:00:00,{close},{close},{close},{close},10\n",
            1 + i / 24,
            i % 24
        );
    }
    fs::write(&file, text).unwrap();
    lab.dispatch(
        "import_data",
        &json!({"files": [file], "symbol": "HANDUSD", "venue": "HAND", "wait_seconds": 60}),
    )
    .unwrap();
    let times = (0..closes.len() as u64)
        .map(|i| JANUARY_2024 + i * HOUR)
        .collect();
    (lab, times)
}

fn on_hand_market(more: Value) -> Value {
    let mut args = json!({
        "symbol": "HANDUSD", "venue": "HAND", "bars": "1h", "from": "2024-01-01",
        "wait_seconds": 120,
        "settings": {"commission": {"on": true, "value": {"percent": 0.1}}},
    });
    for (key, value) in more.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}

#[test]
fn a_backtest_worked_out_by_hand_comes_to_the_same() {
    // Five days: 100, up to 110 at hour 31, then 121 from hour 80.
    let closes: Vec<f64> = (0..120)
        .map(|hour| match hour {
            0..=30 => 100.0,
            31..=79 => 110.0,
            _ => 121.0,
        })
        .collect();
    let (lab, times) = market_of(&closes);
    // Long from hour 10 to 31, which wins; short from hour 60, still open at the end, which loses.
    let run = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": {"positions": [
                [times[10], 1], [times[31], 0], [times[60], -1],
            ]}})),
        )
        .unwrap();
    let summary = &run["summary"];
    let fills = lab
        .dispatch("get_fills", &json!({"run_id": run["run_id"]}))
        .unwrap();
    let rows = fills["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{fills}");
    let close = |value: &Value, expected: f64, what: &str| {
        let value = value.as_f64().unwrap_or_else(|| panic!("{what}: {value}"));
        assert!(
            (value - expected).abs() < 1e-6,
            "{what}: {value} against {expected}"
        );
    };

    // The fills are where the list said: each at its bar's close, and worth 10% of 100,000.
    for (row, (hour, price)) in rows.iter().zip([(10, 100.0), (31, 110.0), (60, 110.0)]) {
        assert_eq!(row[0].as_u64().unwrap(), times[hour] + HOUR);
        close(&row[3], price, "fill price");
    }
    // Sizes are rounded to the market's precision, so they're read rather than assumed.
    let capital = 100_000.0;
    let long = rows[0][4].as_f64().unwrap();
    let short = rows[2][4].as_f64().unwrap();
    assert!((long * 100.0 - 10_000.0).abs() < 50.0, "{long}");
    assert!((short * 110.0 - 10_000.0).abs() < 50.0, "{short}");
    let (long_fee, exit_fee, short_fee) = (
        0.001 * long * 100.0,
        0.001 * long * 110.0,
        0.001 * short * 110.0,
    );
    close(&rows[0][5], long_fee, "first fee");
    let win = long * 10.0 - long_fee - exit_fee;
    let lose = short * (110.0 - 121.0) - short_fee;
    let net = win + lose;
    close(&summary["net_return"], net / capital, "net_return");
    close(&summary["final_equity"], capital + net, "final_equity");
    close(&summary["trades"], 2.0, "trades");
    close(&summary["win_rate"], 0.5, "win_rate");
    close(&summary["profit_factor"], win / -lose, "profit_factor");
    close(&summary["average_trade"], net / 2.0, "average_trade");
    close(&summary["buy_and_hold_return"], 0.21, "buy_and_hold_return");
    // Held through hours 11 to 31 and 61 to 119.
    close(&summary["exposure"], (21.0 + 59.0) / 120.0, "exposure");

    // Each day's last equity: the short opens on day 3, and its loss comes on day 4.
    let days = [
        capital,
        capital - long_fee,
        capital + win,
        capital + win - short_fee,
        capital + win - short_fee - short * 11.0,
        capital + win - short_fee - short * 11.0,
    ];
    let returns: Vec<f64> = days.windows(2).map(|day| day[1] / day[0] - 1.0).collect();
    let mean = returns.iter().sum::<f64>() / 5.0;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / 4.0;
    close(
        &summary["sharpe"],
        mean / variance.sqrt() * 365.0_f64.sqrt(),
        "sharpe",
    );

    // A time that isn't a bar's says so, rather than quietly doing nothing.
    let error = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": {"positions": [[times[3] + 1, 1]]}})),
        )
        .unwrap_err();
    assert!(error.contains("no bar opens at"), "{error}");
    let refused = lab
        .dispatch(
            "save_result",
            &json!({"name": "List", "strategy": {"positions": [[times[3], 1]]}}),
        )
        .unwrap_err();
    assert!(refused.contains("only fits"), "{refused}");
}

#[test]
fn a_tree_exits_on_its_own_entry_and_sees_whats_really_held() {
    // 100, then 105 from hour 20, then falling a little each hour from hour 30.
    let closes: Vec<f64> = (0..80)
        .map(|hour| match hour {
            0..=19 => 100.0,
            20..=29 => 105.0,
            _ => 105.0 - f64::from(hour - 29),
        })
        .collect();
    let (lab, _times) = market_of(&closes);
    let price = |run: &Value| -> Vec<(u64, f64)> {
        let fills = lab
            .dispatch("get_fills", &json!({"run_id": run["run_id"]}))
            .unwrap();
        fills["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (row[1].as_u64().unwrap(), row[3].as_f64().unwrap()))
            .collect()
    };
    // Stop 5% below its entry: 105 less 5.25 is 99.75, first closed under at hour 35.
    let stop = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": {"logic": {
                "long": {"crosses_above": ["close", 102]},
                "flat": {"lt": ["close", {"mul": ["entry_price", 0.95]}]},
            }}})),
        )
        .unwrap();
    assert_eq!(price(&stop), vec![(20, 105.0), (35, 99.0)]);

    // Out after ten bars.
    let timed = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": {"logic": {
                "long": {"crosses_above": ["close", 102]},
                "flat": {"ge": ["bars_held", 10]},
            }}})),
        )
        .unwrap();
    assert_eq!(price(&timed)[1].0, 30);

    // Asking while warming up isn't holding: "buy if not long" still buys on the first bar that
    // can trade, and only that once.
    let warmed = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({
                "from": "2024-01-02",
                "strategy": {"logic": {"long": {"lt": ["position", 1]}}},
            })),
        )
        .unwrap();
    let fills = price(&warmed);
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0].0, 24, "{fills:?}");
}

/// The time an hour after `time`, as a `from` or `to` takes it.
fn next_hour(time: &str) -> String {
    persephone_lab::time_text(persephone_lab::parse_time(&json!(time)).unwrap() + HOUR)
}

#[test]
fn walk_forward_picks_only_on_what_came_before_what_it_tests() {
    let (lab, _dir) = lab();
    let strategy = json!({"builtin": "ema-cross", "params": {"Slow": 40}});
    let variables = json!([{"name": "Fast", "min": 3, "max": 12, "step": 3}]);
    let answer = lab
        .dispatch(
            "run_walk_forward",
            &with(json!({
                "strategy": strategy, "variables": variables, "windows": 3, "train_ratio": 2,
                "from": "2024-01-02", "wait_seconds": 240,
            })),
        )
        .unwrap();
    let windows = answer["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 3);
    let mut total = 0.0;
    let mut last_to: Option<String> = None;
    for window in windows {
        let at = |name: &str| window[name].as_str().unwrap().to_owned();
        assert!(at("train_from") < at("test_from") && at("test_from") < at("test_to"));
        // Unseen stretches follow each other, so nothing's tested twice.
        if let Some(previous) = &last_to {
            assert!(previous < &at("test_from"), "{windows:?}");
        }
        last_to = Some(at("test_to"));

        // Optimizing on the picking stretch alone picks the same: later bars played no part.
        let alone = lab
            .dispatch(
                "run_optimize",
                &with(json!({
                    "strategy": strategy, "variables": variables,
                    "from": at("train_from"), "to": at("test_from"), "wait_seconds": 240,
                })),
            )
            .unwrap();
        assert_eq!(window["values"], alone["top"][0]["values"], "{window}");

        // And its unseen return is that backtest's, on those bars alone.
        let mut chosen = strategy.clone();
        chosen["params"]["Fast"] = window["values"]["Fast"].clone();
        let run = lab
            .dispatch(
                "run_backtest",
                &with(json!({
                    "strategy": chosen, "from": at("test_from"), "to": next_hour(&at("test_to")),
                    "wait_seconds": 240,
                })),
            )
            .unwrap();
        assert!(
            (run["summary"]["net_return"].as_f64().unwrap()
                - window["unseen_return"].as_f64().unwrap())
            .abs()
                < 1e-9,
            "{window} against {run}"
        );
        total += window["unseen_return"].as_f64().unwrap();
    }
    assert!((answer["unseen_return"].as_f64().unwrap() - total).abs() < 1e-9);
    assert!(answer["buy_and_hold_return"].is_number(), "{answer}");
    assert!(windows[0]["buy_and_hold_return"].is_number(), "{answer}");

    let error = lab
        .dispatch(
            "run_walk_forward",
            &with(json!({
                "strategy": strategy, "variables": variables, "windows": 3,
                "from": "2024-01-01", "to": "2024-01-01 06:00", "wait_seconds": 60,
            })),
        )
        .unwrap_err();
    assert!(error.contains("too few"), "{error}");
}

#[test]
fn runs_can_be_labelled_found_and_sorted() {
    let (lab, _dir) = lab();
    let run = |fast: u32, label: &str| {
        lab.dispatch(
            "run_backtest",
            &with(json!({
                "strategy": {"builtin": "ema-cross", "params": {"Fast": fast, "Slow": 40}},
                "label": label, "wait_seconds": 120,
            })),
        )
        .unwrap()
    };
    let runs = [run(3, "a"), run(6, "a"), run(9, "b")];
    let found = lab.dispatch("list_runs", &json!({"label": "a"})).unwrap();
    assert_eq!(found["total"], 2);
    let sorted = lab
        .dispatch("list_runs", &json!({"sort": "net_return"}))
        .unwrap();
    let returns: Vec<f64> = sorted["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["summary"]["net_return"].as_f64().unwrap())
        .collect();
    assert_eq!(returns.len(), runs.len());
    assert!(
        returns.windows(2).all(|pair| pair[0] >= pair[1]),
        "{returns:?}"
    );
    let lowest = lab
        .dispatch(
            "list_runs",
            &json!({"sort": "net_return", "ascending": true, "limit": 1}),
        )
        .unwrap();
    assert_eq!(
        lowest["rows"][0]["summary"]["net_return"],
        json!(returns[2])
    );
    assert!(lowest["rows"][0].get("request").is_none());
}

/// A strategy over a socket: long on every bar.
fn serve_external() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("tcp://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut out = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let message: Value = serde_json::from_str(&line).unwrap();
                    let answer = match message["type"].as_str().unwrap() {
                        "describe" => {
                            json!({"name": "Always long", "description": "Test", "params": []})
                        }
                        "bar" => json!({"target": 1}),
                        _ => json!({}),
                    };
                    writeln!(out, "{answer}").unwrap();
                }
            });
        }
    });
    address
}

#[test]
fn a_program_over_a_socket_can_be_added_and_backtested() {
    let (lab, _dir) = lab();
    let address = serve_external();
    let added = lab
        .dispatch(
            "install_plugin",
            &json!({"name": "always-long", "address": address}),
        )
        .unwrap();
    let id = added["id"].clone();
    assert_eq!(added["kind"], "strategy");
    let listed = lab.dispatch("list_strategies", &json!({})).unwrap();
    assert!(listed.to_string().contains("Always long"), "{listed}");
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": {"plugin": id}, "wait_seconds": 120})),
        )
        .unwrap();
    assert_eq!(run["summary"]["fills"], 1, "{run}");

    // Nothing's left behind if it isn't answering, and an existing one isn't replaced.
    let closed = lab.dispatch(
        "install_plugin",
        &json!({"name": "not-there", "address": "tcp://127.0.0.1:1"}),
    );
    assert!(closed.is_err());
    assert!(!lab.paths.plugins.join("not-there.external").exists());
    let again = lab
        .dispatch(
            "install_plugin",
            &json!({"name": "always-long", "address": address}),
        )
        .unwrap_err();
    assert!(again.contains("already exists"), "{again}");
    let empty = lab.dispatch("install_plugin", &json!({})).unwrap_err();
    assert!(empty.contains("`path`"), "{empty}");
}

#[test]
fn two_labs_sharing_a_folder_download_a_cold_market_once() {
    let source = Arc::new(FakeBinance {
        requests: AtomicUsize::new(0),
    });
    let dir = std::env::temp_dir().join(format!("persephone-lab-shared-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let labs: Vec<Arc<Lab>> = (0..2)
        .map(|_| Lab::with_source(Paths::under(&dir), source.clone()).unwrap())
        .collect();
    let args = json!({"symbol": "BTCUSDT", "bars": "1h", "strategy": ema(), "wait_seconds": 120});
    let answers: Vec<Value> = thread::scope(|scope| {
        let running: Vec<_> = labs
            .iter()
            .map(|lab| scope.spawn(|| lab.dispatch("run_backtest", &args).unwrap()))
            .collect();
        running.into_iter().map(|one| one.join().unwrap()).collect()
    });
    assert_eq!(answers[0]["summary"], answers[1]["summary"]);
    assert_eq!(
        source.requests.load(Ordering::Relaxed),
        5,
        "one download's pages"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn many_kinds_of_call_at_once_all_finish() {
    let (lab, _dir) = lab();
    let stop_at = Instant::now() + Duration::from_secs(4);
    let first = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": ema(), "wait_seconds": 120})),
        )
        .unwrap();
    let id = first["run_id"].clone();
    let calls: Vec<(&str, Value)> = vec![
        (
            "run_backtest",
            with(json!({"strategy": ema(), "wait_seconds": 120})),
        ),
        (
            "run_backtests",
            json!({
                "defaults": market(), "runs": [{"strategy": ema()}, {"strategy": ema()}],
                "wait_seconds": 120,
            }),
        ),
        ("get_report", json!({"run_id": id})),
        ("get_trades", json!({"run_id": id, "limit": 5})),
        (
            "run_monte_carlo",
            json!({"run_id": id, "runs": 100, "wait_seconds": 120}),
        ),
        ("list_runs", json!({"limit": 3})),
        ("list_runs", json!({"sort": "sharpe", "limit": 3})),
        ("get_call_log", json!({"limit": 3})),
        ("get_bars", with(json!({"from": "2024-01-02", "limit": 10}))),
    ];
    let done = AtomicUsize::new(0);
    thread::scope(|scope| {
        for worker in 0..12 {
            let (lab, calls, done) = (&lab, &calls, &done);
            scope.spawn(move || {
                let mut at = worker;
                while Instant::now() < stop_at {
                    let (tool, args) = &calls[at % calls.len()];
                    lab.dispatch(tool, args)
                        .unwrap_or_else(|e| panic!("{tool}: {e}"));
                    done.fetch_add(1, Ordering::Relaxed);
                    at += 1;
                }
                // Starting and cancelling mid-way leaves nothing stuck.
                let job = lab
                    .dispatch(
                        "run_backtests",
                        &json!({
                            "defaults": market(), "runs": vec![json!({"strategy": ema()}); 200],
                            "wait_seconds": 0,
                        }),
                    )
                    .unwrap();
                if let Some(id) = job["job_id"]
                    .as_str()
                    .filter(|_| job["status"] == "running")
                {
                    let _ = lab.dispatch("cancel_job", &json!({"job_id": id}));
                    lab.dispatch("get_job", &json!({"job_id": id, "wait_seconds": 300}))
                        .unwrap();
                }
            });
        }
    });
    assert!(
        done.load(Ordering::Relaxed) > 12,
        "{}",
        done.load(Ordering::Relaxed)
    );
}

/// Against the real Binance, to prove the live format still matches. It needs the network, so it only
/// runs when asked: `cargo test -p persephone-lab -- --ignored`.
#[test]
#[ignore = "needs the network"]
fn real_binance_bars_parse_are_closed_and_cache_exactly() {
    let dir = std::env::temp_dir().join(format!("persephone-lab-real-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let lab = Lab::new(Paths::under(&dir)).unwrap();
    let args = json!({
        "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-03-15", "limit": 5_000,
    });
    let first = lab.dispatch("get_bars", &args).unwrap();
    let rows = first["rows"].as_array().unwrap();
    // Across more than one request, with none missing.
    assert_eq!(rows.len(), 74 * 24);
    for pair in rows.windows(2) {
        assert_eq!(
            pair[1][0].as_u64().unwrap() - pair[0][0].as_u64().unwrap(),
            HOUR
        );
    }
    assert!(
        rows.iter().all(|row| row[2].as_f64().unwrap() > 1_000.0),
        "prices look wrong"
    );

    // From a fresh start the cache gives what Binance did.
    let again = Lab::new(Paths::under(&dir)).unwrap();
    assert_eq!(
        again.dispatch("get_bars", &args).unwrap()["rows"],
        first["rows"]
    );

    // No bar still forming, with no `to`.
    let latest = lab
        .dispatch(
            "get_bars",
            &json!({"symbol": "BTCUSDT", "bars": "1h", "limit": 2}),
        )
        .unwrap();
    let last = latest["rows"][1][0].as_u64().unwrap();
    let now = persephone_lab::now_ms() * 1_000_000;
    assert!(last + HOUR <= now, "{last} is still forming at {now}");

    let run = lab
        .dispatch(
            "run_backtest",
            &json!({
                "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-03-15",
                "strategy": {"builtin": "ema-cross"}, "wait_seconds": 120,
            }),
        )
        .unwrap();
    assert!(
        run["summary"]["buy_and_hold_return"].as_f64().unwrap() > 0.0,
        "{run}"
    );
    assert!(run["summary"]["sharpe"].is_number(), "{run}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn orders_on_their_way_arent_sent_twice() {
    let closes: Vec<f64> = (0..60).map(|hour| 100.0 + f64::from(hour)).collect();
    let (lab, times) = market_of(&closes);
    // Latency on these bars delays a fill to the next bar, where a list still asks for the same.
    let run = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({
                "strategy": {"positions": [[times[10], 1], [times[20], -1], [times[30], 0]]},
                "settings": {"latency": {"on": true, "value": 100}},
            })),
        )
        .unwrap();
    let fills = lab
        .dispatch("get_fills", &json!({"run_id": run["run_id"]}))
        .unwrap();
    let rows = fills["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{fills}");
    assert_eq!(run["summary"]["trades"], 2, "{run}");
    // One order for each change, a bar later than it would have been.
    for (row, (hour, side)) in rows.iter().zip([(11, "buy"), (21, "sell"), (31, "buy")]) {
        assert_eq!(row[1], hour, "{row}");
        assert_eq!(row[2], side, "{row}");
    }
    assert!(run["summary"]["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn refused_orders_are_reported_and_standard_output_stays_json() {
    // A short that tripled against it lost more than the account holds, so it can't open another.
    let closes: Vec<f64> = (0..60)
        .map(|hour| if hour < 10 { 100.0 } else { 300.0 })
        .collect();
    let (lab, times) = market_of(&closes);
    let args = on_hand_market(json!({
        "strategy": {
            "positions": [[times[5], -1], [times[20], 0], [times[30], 1]],
            "params": {"Size %": 100},
        },
    }));
    let run = lab.dispatch("run_backtest", &args).unwrap();
    let warnings = run["summary"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{run}");
    assert!(warnings[0].as_str().unwrap().contains("refused"), "{run}");

    // What reaches standard output is what a client parses.
    let dir = lab.paths.lab().parent().unwrap().to_owned();
    let output = Command::new(env!("CARGO_BIN_EXE_persephone-lab"))
        .args(["--data"])
        .arg(&dir)
        .args(["call", "run_backtest"])
        .arg(args.to_string())
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let parsed: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("not JSON: {e}: {}", &text[..text.len().min(300)]));
    assert_eq!(parsed["summary"]["warnings"], run["summary"]["warnings"]);

    // A long list can come from a file.
    let file = dir.join("args.json");
    fs::write(&file, args.to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_persephone-lab"))
        .args(["--data"])
        .arg(&dir)
        .args(["call", "run_backtest"])
        .arg(format!("@{}", file.display()))
        .output()
        .unwrap();
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        parsed["summary"]["net_return"],
        run["summary"]["net_return"]
    );
}

#[test]
fn what_would_quietly_do_nothing_says_so() {
    let (lab, _dir) = lab();
    let tried = |more: Value| {
        lab.dispatch(
            "run_backtest",
            &with(json!({"strategy": ema(), "settings": more, "wait_seconds": 60})),
        )
    };
    let error = tried(json!({"slippagee": {"on": true, "value": 100}})).unwrap_err();
    assert!(
        error.contains("`slippagee`") && error.contains("slippage"),
        "{error}"
    );
    let error = tried(json!({"slippage": {"value": 100}})).unwrap_err();
    assert!(error.contains("\"on\""), "{error}");
    assert!(tried(json!({"slippage": {"on": true, "value": 100}})).is_ok());

    let error = lab
        .dispatch(
            "run_backtest",
            &with(json!({"wait_seconds": 60, "strategy": {"logic": {
                "params": {"Size %": 100}, "long": {"gt": ["close", 1]},
            }}})),
        )
        .unwrap_err();
    assert!(error.contains("beside `logic`"), "{error}");
}

#[test]
fn a_list_of_positions_says_what_to_hold_wherever_a_run_starts() {
    let closes: Vec<f64> = (0..600).map(|hour| 100.0 + f64::from(hour) * 0.1).collect();
    let (lab, times) = market_of(&closes);
    let hold = json!({"positions": [[times[0], 1]]});
    // A span starting after its time takes it up on the first bar it can trade.
    let later = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": hold, "from": "2024-01-03"})),
        )
        .unwrap();
    assert_eq!(later["summary"]["fills"], 1, "{later}");
    assert!(
        later["summary"]["net_return"].as_f64().unwrap() > 0.0,
        "{later}"
    );

    // And so does each random block, which reports what the market did beside it.
    let blocks = lab
        .dispatch(
            "run_random_blocks",
            &on_hand_market(json!({
                "strategy": hold, "blocks": 4, "length": "week", "seed": 1,
            })),
        )
        .unwrap();
    for block in blocks["blocks"].as_array().unwrap() {
        assert_eq!(block["fills"], 1, "{block}");
        assert!(
            block["buy_and_hold_return"].as_f64().unwrap() > 0.0,
            "{block}"
        );
        assert!(block["return"].as_f64().unwrap() > 0.0, "{block}");
    }
    assert!(blocks["median_buy_and_hold"].as_f64().unwrap() > 0.0);
}

#[test]
fn a_trailing_stop_follows_the_high_since_entry() {
    // Up from 100 to 130, then falling, with the bar's high 1 above its close.
    let closes: Vec<f64> = (0..80)
        .map(|hour| match hour {
            0..=10 => 100.0,
            11..=40 => 100.0 + f64::from(hour - 10),
            _ => 130.0 - f64::from(hour - 40),
        })
        .collect();
    let (lab, _times) = market_of(&closes);
    let run = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": {"logic": {
                "long": {"crosses_above": ["close", 100.5]},
                "flat": {"lt": ["close", {"mul": ["high_since_entry", 0.9]}]},
            }}})),
        )
        .unwrap();
    let fills = lab
        .dispatch("get_fills", &json!({"run_id": run["run_id"]}))
        .unwrap();
    // 10% under the high of 130 is 117, first closed under at 116.
    assert_eq!(fills["rows"][0][3], 101.0, "{fills}");
    assert_eq!(fills["rows"][1][3], 116.0, "{fills}");
    assert_eq!(fills["rows"].as_array().unwrap().len(), 2, "{fills}");
}

#[test]
fn the_written_reference_matches_the_tools() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/lab-tools.md");
    let written = fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        written,
        reference(),
        "run: persephone-lab tools --markdown > docs/lab-tools.md"
    );
}

#[test]
fn lab_is_built_without_live_trading() {
    // Cargo's own view of Lab's dependencies, leaving out what only its tests use.
    static TREE: OnceLock<String> = OnceLock::new();
    let tree = TREE.get_or_init(|| {
        let output = Command::new(env!("CARGO"))
            .args([
                "tree",
                "-p",
                "persephone-lab",
                "-e",
                "normal,features",
                "--offline",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    });
    assert!(tree.contains("persephone-engine"), "it uses the engine");
    assert!(!tree.contains("persephone-engine feature \"live\""));
    assert!(!tree.contains("nautilus-live"));
    assert!(!tree.contains("nautilus-binance"));
}
