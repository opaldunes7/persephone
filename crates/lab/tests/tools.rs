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
    Lab, Paths, http,
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
