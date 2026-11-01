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
    let basket = json!({"legs": [{"symbol": "BTCUSDT", "venue": "TEST", "weight": 0.5}], "bars": "1h", "wait_seconds": 120});
    let portfolio = lab.dispatch("run_portfolio", &basket).unwrap();
    let logged = lab
        .dispatch(
            "run_backtest",
            &with(
                json!({"strategy": ema(), "settings": {"decision_log": true}, "wait_seconds": 120}),
            ),
        )
        .unwrap();
    let portfolio_id = portfolio["run_id"].clone();
    vec![
        ("run_portfolio", basket.clone()),
        (
            "run_portfolios",
            json!({"runs": [basket], "wait_seconds": 120}),
        ),
        (
            "run_shifted",
            json!({"run_id": portfolio_id, "runs": 3, "wait_seconds": 120}),
        ),
        (
            "run_portfolio_monte_carlo",
            json!({"run_id": portfolio_id, "runs": 10, "wait_seconds": 60}),
        ),
        (
            "run_portfolio_walk_forward",
            json!({
                "candidates": [{"legs": [{"symbol": "BTCUSDT", "venue": "TEST", "weight": 0.5}], "bars": "1h"}],
                "windows": 2,
                "wait_seconds": 120,
            }),
        ),
        ("get_decisions", json!({"run_id": logged["run_id"]})),
        ("get_portfolio", json!({"run_id": portfolio_id})),
        (
            "get_portfolio_series",
            json!({"run_id": portfolio_id, "limit": 2}),
        ),
        (
            "get_portfolio_fills",
            json!({"run_id": portfolio_id, "symbol": "BTCUSDT"}),
        ),
        ("get_portfolio_trades", json!({"run_id": portfolio_id})),
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
        ("get_news", json!({})),
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
            "run_random_entries",
            json!({"run_id": run_id, "runs": 5, "wait_seconds": 120}),
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
        ("get_limit_orders", json!({"run_id": run_id})),
        ("get_equity", json!({"run_id": run_id, "limit": 5})),
        ("get_returns", json!({"run_id": run_id})),
        ("get_calendar", json!({"run_id": run_id})),
        ("list_runs", json!({})),
        ("compare_runs", json!({"run_ids": [run_id]})),
        ("get_job", json!({"job_id": job["job_id"]})),
        ("get_layout", json!({})),
        ("get_chart_settings", json!({})),
        ("get_drawings", json!({})),
        ("export_state", json!({"path": dir.join("state.json")})),
        ("import_state", json!({"path": dir.join("state.json")})),
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
const NOT_CALLED_HERE: [&str; 6] = [
    "get_ticks",
    "import_data",
    "fetch_archive",
    // Needs a perpetual, which the local market in these calls isn't; has its own test.
    "get_funding",
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
    let mut unlisted = Vec::new();
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
        // And everything it answers is listed, so an agent reading the tools knows it's there.
        if let Some(keys) = answer.as_object() {
            unlisted.extend(
                keys.keys()
                    // What a job's call is, said of every job, by the job.
                    .filter(|key| *key != "job_id")
                    .filter(|key| !tool.returns.contains(&key.as_str()))
                    .map(|key| format!("{name}.{key}")),
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
    assert!(unlisted.is_empty(), "answered but not listed: {unlisted:?}");
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
        persephone_research::WARM_UP_BARS,
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
        funding: None,
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
    let stopped = loop {
        let status = lab.dispatch("get_job", &json!({"job_id": id})).unwrap();
        assert_ne!(status["status"], "done", "it should've stopped: {status}");
        if status["status"] == "cancelled" {
            break status;
        }
        assert!(Instant::now() < deadline, "it never stopped");
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(stopped["job_id"], id, "{stopped}");
    assert_eq!(stopped["total"], 400, "{stopped}");
    // It reads the same once only its file is left, as after a restart.
    let file = lab
        .paths
        .lab()
        .join("jobs")
        .join(format!("{}.json", id.as_str().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !file.exists() {
        assert!(Instant::now() < deadline, "it was never saved");
        thread::sleep(Duration::from_millis(20));
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(saved["status"], "cancelled", "{saved}");

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
    funding_requests: AtomicUsize,
    requests: AtomicUsize,
    /// Hours from `START` with no bars, as an exchange outage leaves.
    hole: Option<(u64, u64)>,
}

const HOURS: u64 = 3_000;

impl BarSource for FakeBinance {
    fn funding(&self, symbol: &str, start: u64, end: u64) -> Result<Vec<(u64, f64)>, String> {
        if !symbol.ends_with("-PERP") {
            return Ok(Vec::new());
        }
        self.funding_requests.fetch_add(1, Ordering::Relaxed);
        let every = 8 * HOUR;
        Ok((start.div_ceil(every)..)
            .map(|step| step * every)
            .take_while(|when| *when < end)
            .map(|when| (when, 0.0001))
            .collect())
    }

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
            .filter(|hour| {
                self.hole.is_none_or(|(from, to)| {
                    !(START / HOUR + from..START / HOUR + to).contains(hour)
                })
            })
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
        funding_requests: AtomicUsize::new(0),
        requests: AtomicUsize::new(0),
        hole: None,
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

#[test]
fn a_run_holding_to_the_end_reports_its_open_trade_as_the_summary_does() {
    // Up 10% from hour 50 with a long held from hour 10 to the end, never closed.
    let closes: Vec<f64> = (0..120)
        .map(|h| if h < 50 { 100.0 } else { 110.0 })
        .collect();
    let (lab, times) = market_of(&closes);
    let run = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": {"positions": [[times[10], 1]]}})),
        )
        .unwrap();
    let report = lab
        .dispatch("get_report", &json!({"run_id": run["run_id"]}))
        .unwrap();
    let summary = &report["summary"];
    let pnl = &report["pnl"];
    assert_eq!(report["ends_with_open_trade"], true, "{report}");
    let profit =
        summary["final_equity"].as_f64().unwrap() - report["starting_balance"].as_f64().unwrap();
    assert!(profit > 0.0, "{summary}");
    assert!(
        (pnl["PnL (total)"].as_f64().unwrap() - profit).abs() < 1e-6,
        "{pnl}"
    );
    assert_eq!(pnl["Win Rate"], summary["win_rate"], "{pnl}");
    assert_eq!(pnl["Max Winner"], pnl["PnL (total)"], "{pnl}");
    assert!(pnl["Avg Loser"].is_null(), "{pnl}");
    // The engine's own leaves it out, and is kept under a name saying so.
    assert!(
        report["closed_positions_pnl"]["PnL (total)"]
            .as_f64()
            .unwrap()
            <= 0.0,
        "{report}"
    );
}

#[test]
fn a_line_that_is_not_json_gets_an_error_with_a_null_id_and_the_server_carries_on() {
    let dir = temp_dir();
    let mut client = McpClient::start(&dir);
    let stdin = client.child.stdin.as_mut().unwrap();
    writeln!(stdin, "{{not json").unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    client.output.read_line(&mut line).unwrap();
    let answer: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(answer["error"]["code"], -32700, "{answer}");
    assert!(answer.get("id").is_some_and(Value::is_null), "{answer}");
    assert!(client.request("tools/list", json!({}))["tools"].is_array());
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
    let docs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let text = fs::read_to_string(docs.join("lab-agents.md")).unwrap()
        + &fs::read_to_string(docs.join("lab-external-models.md")).unwrap();
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

    // A list saved for the app's charts is written out, in case it came from a file.
    let file = lab.paths.lab().join("list.json");
    fs::write(&file, json!([[times[3], 1]]).to_string()).unwrap();
    lab.dispatch(
        "save_result",
        &json!({"name": "List", "strategy": {"positions": file}}),
    )
    .unwrap();
    let saved = lab.dispatch("list_results", &json!({})).unwrap();
    assert_eq!(
        saved["results"][0]["strategy"]["positions"][0][1], 1,
        "{saved}"
    );
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
        // Fast at 3, 6, 9 and 12 is four tried, and the best's Sharpe ratio is deflated for it.
        assert_eq!(alone["variants_tried"], 4);
        let picked = alone["top"][0]["sharpe"].as_f64();
        let deflated = alone["deflated_sharpe"].as_f64();
        assert_eq!(picked.is_some(), deflated.is_some(), "{alone}");
        if let Some(deflated) = deflated {
            assert!((0.0..=1.0).contains(&deflated), "{alone}");
        }
        assert_eq!(
            window["picked_sharpe"], alone["top"][0]["sharpe"],
            "{window}"
        );
        assert_eq!(
            window["deflated_sharpe"], alone["deflated_sharpe"],
            "{window}"
        );

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
    // The market held once over all the unseen bars is a backtest's market over them.
    let whole = lab
        .dispatch(
            "run_backtest",
            &with(json!({
                "strategy": strategy,
                "from": windows[0]["test_from"],
                "to": next_hour(windows[2]["test_to"].as_str().unwrap()),
                "wait_seconds": 240,
            })),
        )
        .unwrap();
    assert!(
        (answer["buy_and_hold_whole"].as_f64().unwrap()
            - whole["summary"]["buy_and_hold_return"].as_f64().unwrap())
        .abs()
            < 1e-9,
        "{answer} against {whole}"
    );

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
                // Answers go at once, as a real one's should: otherwise every bar waits on the
                // other side's delayed acknowledgement.
                stream.set_nodelay(true).unwrap();
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
                    out.write_all(format!("{answer}\n").as_bytes()).unwrap();
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
        funding_requests: AtomicUsize::new(0),
        requests: AtomicUsize::new(0),
        hole: None,
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
fn real_binance_perpetuals_parse_and_pay_real_funding() {
    let dir = std::env::temp_dir().join(format!("persephone-lab-perp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let lab = Lab::new(Paths::under(&dir)).unwrap();
    let market =
        json!({"symbol": "BTCUSDT-PERP", "bars": "1h", "from": "2024-01-01", "to": "2024-03-01"});
    let info = lab
        .dispatch("get_instrument", &json!({"symbol": "BTCUSDT-PERP"}))
        .unwrap();
    assert_eq!(info["fees"]["taker"], 0.0005, "{info}");
    let mut args = market.clone();
    args["strategy"] =
        json!({"positions": [[1_704_067_200_000_000_000_u64, 1]], "params": {"Size %": 100}});
    args["wait_seconds"] = json!(120);
    let run = lab.dispatch("run_backtest", &args).unwrap();
    // Funding on BTC was well above zero then, every eight hours, and a long paid it.
    assert!(
        run["summary"]["funding_paid"].as_f64().unwrap() > 0.0,
        "{run}"
    );
    assert_eq!(run["summary"]["warnings"], json!([]), "{run}");
}

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
    assert_eq!(warnings.len(), 2, "{run}");
    assert!(warnings[0].as_str().unwrap().contains("refused"), "{run}");
    assert!(warnings[1].as_str().unwrap().contains("depleted"), "{run}");

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
fn slippage_as_a_percentage_and_compounding_work_out_by_hand() {
    let closes: Vec<f64> = (0..60)
        .map(|hour| if hour < 20 { 100.0 } else { 200.0 })
        .collect();
    let (lab, times) = market_of(&closes);
    let fills_of = |run: &Value| {
        let fills = lab
            .dispatch("get_fills", &json!({"run_id": run["run_id"]}))
            .unwrap();
        fills["rows"].as_array().unwrap().clone()
    };
    let list = json!({"positions": [
        [times[5], 1], [times[25], 0], [times[30], 1], [times[40], 0],
    ]});
    let no_fees = json!({"commission": {"on": true, "value": {"percent": 0}}});

    // 1% of the price against each fill, on any market.
    let mut settings = no_fees.clone();
    settings["slippage_percent"] = json!({"on": true, "value": 1.0});
    let run = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": list, "settings": settings})),
        )
        .unwrap();
    let fills = fills_of(&run);
    assert_eq!(fills[0][3], 101.0, "a buy pays more");
    assert_eq!(fills[1][3], 198.0, "a sell gets less");

    // Without compounding, the second position is worth the same as the first.
    let flat = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": list, "settings": no_fees})),
        )
        .unwrap();
    let fills = fills_of(&flat);
    let worth = |fill: &Value| fill[3].as_f64().unwrap() * fill[4].as_f64().unwrap();
    assert!((worth(&fills[0]) - 10_000.0).abs() < 1.0);
    assert!((worth(&fills[2]) - 10_000.0).abs() < 1.0);

    // With it, the first doubled 10% of the account, so the second is 10% of 110,000.
    let mut settings = no_fees.clone();
    settings["compound"] = json!(true);
    let grown = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": list, "settings": settings})),
        )
        .unwrap();
    let fills = fills_of(&grown);
    assert!((worth(&fills[0]) - 10_000.0).abs() < 1.0, "{fills:?}");
    assert!((worth(&fills[2]) - 11_000.0).abs() < 1.0, "{fills:?}");
}

#[test]
fn optimizing_with_bars_kept_back_says_what_the_market_did_on_them() {
    let closes: Vec<f64> = (0..600).map(|hour| 100.0 + f64::from(hour) * 0.1).collect();
    let (lab, _times) = market_of(&closes);
    let answer = lab
        .dispatch(
            "run_optimize",
            &on_hand_market(json!({
                "strategy": {"builtin": "ema-cross"}, "hold_out": true,
                "variables": [{"name": "Fast", "min": 3, "max": 6, "step": 3}],
            })),
        )
        .unwrap();
    // The last 30% of 600 bars: from bar 420 at 142, to bar 599 at 159.9.
    let held = answer["held_out_buy_and_hold"].as_f64().unwrap();
    assert!((held - (159.9 / 142.0 - 1.0)).abs() < 1e-9, "{held}");
}

#[test]
fn every_position_asked_for_is_taken_however_sizes_round() {
    // Flips leave sizes that don't add back to exactly zero in floating point, which once read as
    // still being short, so the next short never went out.
    let (lab, _source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let cycle = [1, -1, 0, -1, 0, 1, 0, -1, 1, 0];
    let list: Vec<Value> = (0..250)
        .map(|i| json!([at(30 + i * 11), cycle[i as usize % cycle.len()]]))
        .collect();
    let run = lab
        .dispatch(
            "run_backtest",
            &json!({
                "symbol": "BTCUSDT", "bars": "1h", "from": at(0), "wait_seconds": 120,
                "strategy": {"positions": list},
            }),
        )
        .unwrap();
    assert_eq!(run["summary"]["fills"], 250, "{run}");
    // Sizes come back as what was held, each flat really being flat.
    let fills = lab
        .dispatch(
            "get_fills",
            &json!({"run_id": run["run_id"], "limit": 1000}),
        )
        .unwrap();
    let sides: Vec<&str> = fills["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[2].as_str().unwrap())
        .collect();
    let expected: Vec<&str> = (0..250)
        .map(|i| {
            let now = cycle[i % cycle.len()];
            let before = if i == 0 {
                0
            } else {
                cycle[(i - 1) % cycle.len()]
            };
            if now > before { "buy" } else { "sell" }
        })
        .collect();
    assert_eq!(sides, expected);
    assert_eq!(run["summary"]["warnings"], json!([]), "{run}");
}

#[test]
fn a_run_following_its_strategy_says_nothing_however_it_is_set_up() {
    let (lab, _source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let cycle = [1, -1, 0, -1, 0, 1, 0, -1, 1, 0];
    let list: Vec<Value> = (0..120)
        .map(|i| json!([at(30 + i * 11), cycle[i as usize % cycle.len()]]))
        .collect();
    let stop = json!({"logic": {
        "name": "Trailing",
        "long": {"crosses_above": [{"ema": {"length": 5}}, {"ema": {"length": 15}}]},
        "short": {"crosses_below": [{"ema": {"length": 5}}, {"ema": {"length": 15}}]},
        "flat": {"lt": ["close", {"mul": ["high_since_entry", 0.99]}]},
    }});
    let costs = json!({
        "commission": {"on": true, "value": {"percent": 0.1}},
        "slippage_percent": {"on": true, "value": 0.05},
    });
    let settings = [
        json!({}),
        costs.clone(),
        json!({"latency": {"on": true, "value": 100}}),
        json!({"compound": true, "commission": costs["commission"], "slippage_percent": costs["slippage_percent"]}),
        json!({"direction": {"on": true, "value": "long-only"}}),
        json!({"direction": {"on": true, "value": "short-only"}}),
        json!({"invert": true}),
        json!({"leverage": {"on": true, "value": 2}}),
        json!({"session": {"on": true, "value": {
            "days": [true, true, true, true, true, false, false],
            "from": 570, "to": 960, "zone": "UTC", "close_after": true,
        }}}),
        json!({"session": {"on": true, "value": {
            "days": [true, true, true, true, true, true, true],
            "from": 60, "to": 600, "zone": "UTC", "close_after": false,
        }}}),
    ];
    for strategy in [
        json!({"positions": list}),
        stop,
        json!({"builtin": "ema-cross"}),
        json!({"builtin": "rsi-reversion"}),
    ] {
        for settings in &settings {
            let run = lab
                .dispatch(
                    "run_backtest",
                    &json!({
                        "symbol": "BTCUSDT", "bars": "1h", "from": at(0), "wait_seconds": 120,
                        "strategy": strategy, "settings": settings,
                    }),
                )
                .unwrap();
            let warnings = run["summary"]["warnings"].as_array().unwrap();
            assert!(
                !warnings.iter().any(|w| {
                    let text = w.as_str().unwrap();
                    text.contains("wasn't what") || text.contains("rounds to nothing")
                }),
                "{settings}: {warnings:?}"
            );
        }
    }
}

#[test]
fn a_position_too_small_to_order_says_so() {
    // Whole units here, so 1,000 is 10 at 100, the size checked at the start from the last close, but
    // rounds to nothing at 100,000 partway through.
    let closes: Vec<f64> = (0..120)
        .map(|hour| {
            if (50..70).contains(&hour) {
                100_000.0
            } else {
                100.0
            }
        })
        .collect();
    let (lab, times) = market_of(&closes);
    let run = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({
                "strategy": {
                    "positions": [[times[10], 1], [times[20], 0], [times[60], 1], [times[65], 0]],
                    "params": {"Size %": 1},
                },
            })),
        )
        .unwrap();
    assert_eq!(run["summary"]["fills"], 2, "{run}");
    let warnings = run["summary"]["warnings"].to_string();
    assert!(warnings.contains("On 5 bars"), "{warnings}");
    assert!(warnings.contains("rounds to nothing"), "{warnings}");
    assert!(!warnings.contains("wasn't what"), "{warnings}");
}

#[test]
fn with_latency_every_change_goes_out_a_bar_late_and_the_position_stays_right() {
    let closes: Vec<f64> = (0..120).map(|hour| 100.0 + f64::from(hour % 7)).collect();
    let (lab, times) = market_of(&closes);
    let run_with = |list: &[(usize, i64)], settings: Value| {
        let list: Vec<Value> = list
            .iter()
            .map(|(hour, want)| json!([times[*hour], want]))
            .collect();
        lab.dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": {"positions": list}, "settings": settings})),
        )
        .unwrap()
    };
    let latency = json!({"latency": {"on": true, "value": 100}});
    let cycle = [1, -1, 0, -1, 0, 1, 0, -1, 1, 0];
    // A change on every bar, with flips, entries and exits.
    let list: Vec<(usize, i64)> = (0..40).map(|i| (10 + i, cycle[i % cycle.len()])).collect();

    let fills_of = |run: &Value| {
        let fills = lab
            .dispatch(
                "get_fills",
                &json!({"run_id": run["run_id"], "limit": 1000}),
            )
            .unwrap();
        fills["rows"].as_array().unwrap().clone()
    };
    let prompt = run_with(&list, json!({}));
    let late = run_with(&list, latency.clone());
    assert_eq!(late["summary"]["warnings"], json!([]), "{late}");
    let (prompt, late) = (fills_of(&prompt), fills_of(&late));
    assert_eq!(prompt.len(), 40);
    assert_eq!(late.len(), 40, "{late:?}");
    let mut position = 0.0;
    for ((early, late), (_, want)) in prompt.iter().zip(&late).zip(&list) {
        assert_eq!(late[0].as_u64().unwrap(), early[0].as_u64().unwrap() + HOUR);
        assert_eq!(late[2], early[2], "same side");
        assert_eq!(late[4], early[4], "same size");
        // Never doubled: after each it's what was asked for.
        let size = late[4].as_f64().unwrap();
        position += if late[2] == "buy" { size } else { -size };
        assert_eq!(position.signum() as i64 * i64::from(position != 0.0), *want);
    }

    // With nothing to spare: each fill is paid for by the one before it, not filled yet.
    let all_in = {
        let list: Vec<Value> = list
            .iter()
            .map(|(hour, want)| json!([times[*hour], want]))
            .collect();
        lab.dispatch(
            "run_backtest",
            &on_hand_market(json!({
                "strategy": {"positions": list, "params": {"Size %": 100}},
                "settings": latency,
            })),
        )
        .unwrap()
    };
    assert_eq!(all_in["summary"]["fills"], 40, "{all_in}");
    assert_eq!(all_in["summary"]["warnings"], json!([]), "{all_in}");
}

#[test]
fn a_list_can_run_past_the_span_and_returns_leave_out_warm_up() {
    let (lab, _source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let from = at(1_000);
    let run = lab
        .dispatch(
            "run_backtest",
            &json!({
                "symbol": "BTCUSDT", "bars": "1h", "from": from, "to": at(1_200), "wait_seconds": 120,
                // Before all the bars loaded, inside the span, and after it.
                "strategy": {"positions": [[at(0), 1], [at(1_100), 0], [at(2_000), 1]]},
            }),
        )
        .unwrap();
    // Taken up at the first bar, closed once, and nothing from what's after.
    assert_eq!(run["summary"]["fills"], 2, "{run}");
    let returns = lab
        .dispatch(
            "get_returns",
            &json!({"run_id": run["run_id"], "limit": 1000}),
        )
        .unwrap();
    let rows = returns["rows"].as_array().unwrap();
    assert!(!rows.is_empty(), "{returns}");
    assert!(
        rows.iter().all(|row| row[0].as_u64().unwrap() >= from),
        "{returns}"
    );
}

#[test]
fn what_costs_took_comes_out_as_worked_by_hand() {
    // Bought at about 100, sold at about 120, both legs slipping 1% and paying a 0.1% fee. In
    // thousandths, as a market in whole units would round the slippage away.
    let closes: Vec<f64> = (0..60)
        .map(|h| if h < 30 { 100.125 } else { 120.125 })
        .collect();
    let (lab, times) = market_of(&closes);
    let list = json!([[times[10], 1], [times[40], 0]]);
    let args = |settings: Value, more: Value| {
        let mut args =
            on_hand_market(json!({"strategy": {"positions": list}, "settings": settings}));
        for (key, value) in more.as_object().unwrap() {
            args[key] = value.clone();
        }
        args
    };
    let costs = json!({
        "commission": {"on": true, "value": {"percent": 0.1}},
        "slippage_percent": {"on": true, "value": 1},
    });
    let run = lab
        .dispatch("run_backtest", &args(costs.clone(), json!({})))
        .unwrap();
    let summary = &run["summary"];
    let close = |value: &Value, expected: f64, what: &str| {
        let value = value.as_f64().unwrap_or_else(|| panic!("{what}: {value}"));
        assert!(
            (value - expected).abs() < 1e-2,
            "{what}: {value} against {expected}"
        );
    };
    let fills = lab
        .dispatch("get_fills", &json!({"run_id": run["run_id"]}))
        .unwrap();
    let qty = fills["rows"][0][4].as_f64().unwrap();
    let bought = fills["rows"][0][3].as_f64().unwrap();
    let sold = fills["rows"][1][3].as_f64().unwrap();
    assert!((bought - 100.125 * 1.01).abs() < 0.001, "{fills}");
    assert!((sold - 120.125 * 0.99).abs() < 0.001, "{fills}");
    let fees = qty * (bought + sold) * 0.001;
    let slipped = qty * (100.125 * 0.01 + 120.125 * 0.01);
    close(&summary["fees"], fees, "fees");
    assert!(
        (summary["total_costs_paid"].as_f64().unwrap() - (fees + slipped)).abs() < 1.0,
        "{summary}"
    );
    // By kind: it's all slippage and fees here, and the kinds add up to the total.
    close(&summary["slippage_paid"], slipped, "slippage");
    for nothing in ["spread_paid", "impact_paid", "range_slippage_paid"] {
        close(&summary[nothing], 0.0, nothing);
    }
    let parts: f64 = [
        "maker_fees",
        "taker_fees",
        "slippage_paid",
        "spread_paid",
        "range_slippage_paid",
        "impact_paid",
        "funding_paid",
        "borrow_paid",
        "cost_rounding",
    ]
    .iter()
    .map(|name| summary[*name].as_f64().unwrap())
    .sum();
    assert!(
        (parts - summary["total_costs_paid"].as_f64().unwrap()).abs() < 1e-9,
        "{summary}"
    );
    assert!(summary["cost_rounding"].as_f64().unwrap().abs() < 0.01);
    // Without costs: the same run, at 100 and 120.
    let free = lab
        .dispatch(
            "run_backtest",
            &args(
                json!({"commission": {"on": true, "value": {"percent": 0}}}),
                json!({}),
            ),
        )
        .unwrap();
    assert_eq!(
        summary["gross_return"], free["summary"]["net_return"],
        "{run}"
    );
    assert_eq!(
        free["summary"]["gross_return"],
        free["summary"]["net_return"]
    );
    close(&free["summary"]["total_costs_paid"], 0.0, "no costs");

    // Charging the break-even on every fill leaves about nothing.
    let bp = summary["break_even_cost_per_fill"].as_f64().unwrap();
    assert!(bp > 0.0, "{summary}");
    let at_break_even = lab
        .dispatch(
            "run_backtest",
            &args(
                json!({"commission": {"on": true, "value": {"percent": bp / 100.0}}}),
                json!({}),
            ),
        )
        .unwrap();
    assert!(
        at_break_even["summary"]["net_return"]
            .as_f64()
            .unwrap()
            .abs()
            < 1e-3,
        "{at_break_even}"
    );

    let skipped = lab
        .dispatch("run_backtest", &args(costs, json!({"friction": false})))
        .unwrap();
    for figure in [
        "gross_return",
        "total_costs_paid",
        "break_even_cost_per_fill",
    ] {
        assert!(skipped["summary"][figure].is_null(), "{figure}: {skipped}");
    }

    // The benchmark, from the first close that could trade, never falling.
    let benchmark = &summary["benchmark"];
    close(
        &benchmark["return"],
        120.125 / 100.125 - 1.0,
        "benchmark return",
    );
    close(&benchmark["max_drawdown"], 0.0, "benchmark fall");
    assert_eq!(benchmark["return"], summary["buy_and_hold_return"]);
    assert!(benchmark["sharpe"].is_number(), "{benchmark}");
}

#[test]
fn a_wiped_out_account_still_shows_what_it_made_before_costs_and_says_it_ran_out() {
    let closes: Vec<f64> = (0..120).map(|h| 100.0 + f64::from(h % 3)).collect();
    let (lab, times) = market_of(&closes);
    let list: Vec<Value> = (0..100)
        .map(|i| json!([times[10 + i], (i % 2) as u8]))
        .collect();
    let run = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({
                "strategy": {"positions": list, "params": {"Size %": 100}},
                "settings": {
                    "compound": true,
                    "capital": 1000,
                    "commission": {"on": true, "value": {"percent": 10}},
                },
            })),
        )
        .unwrap();
    let summary = &run["summary"];
    assert!(summary["net_return"].as_f64().unwrap() < -0.9, "{summary}");
    assert!(
        summary["gross_return"].as_f64().unwrap() > -0.5,
        "{summary}"
    );
    let warnings = summary["warnings"].to_string();
    assert!(
        warnings.contains("Account balance depleted ("),
        "{warnings}"
    );
    assert!(
        warnings.contains("Unable to open further positions"),
        "{warnings}"
    );
    assert!(!warnings.contains("rounds to nothing"), "{warnings}");

    // Monte Carlo draws it as returns, so nothing falls past everything.
    let drawn = lab
        .dispatch(
            "run_monte_carlo",
            &json!({"run_id": run["run_id"], "runs": 200, "seed": 1}),
        )
        .unwrap();
    assert!(drawn["low"].as_f64().unwrap() >= -1.0, "{drawn}");
    assert!(drawn.get("warning").is_none(), "{drawn}");
}

#[test]
fn filling_at_the_next_open_does_as_worked_by_hand() {
    // Each bar opens 3 under where it closes, and well away from the last close.
    let mut text = String::from("time,open,high,low,close,volume\n");
    let closes: Vec<f64> = (0..80).map(|h| 200.0 + f64::from(h % 10) * 2.0).collect();
    for (i, close) in closes.iter().enumerate() {
        let open = close - 3.0;
        let (day, hour) = (1 + i / 24, i % 24);
        text += &format!("2024-01-{day:02} {hour:02}:00:00,{open},{close},{open},{close},10\n");
    }
    let (lab, dir) = lab();
    let file = dir.join("opens.csv");
    fs::write(&file, text).unwrap();
    lab.dispatch(
        "import_data",
        &json!({"files": [file], "symbol": "HANDUSD", "venue": "HAND", "wait_seconds": 60}),
    )
    .unwrap();
    let time = |hour: usize| JANUARY_2024 + hour as u64 * HOUR;
    let changes: Vec<(usize, i64)> = vec![(10, 1), (11, -1), (12, 0), (20, -1), (30, 1), (40, 0)];
    let list: Vec<Value> = changes
        .iter()
        .map(|(hour, want)| json!([time(*hour), want]))
        .collect();
    let run = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({
                "strategy": {"positions": list},
                "settings": {"fill_timing": "next_open"},
            })),
        )
        .unwrap();
    assert_eq!(run["summary"]["warnings"], json!([]), "{run}");
    let fills = lab
        .dispatch("get_fills", &json!({"run_id": run["run_id"], "limit": 100}))
        .unwrap();
    let rows = fills["rows"].as_array().unwrap();
    assert_eq!(rows.len(), changes.len(), "{fills}");
    for (row, (hour, _)) in rows.iter().zip(&changes) {
        let next = hour + 1;
        assert_eq!(row[0].as_u64().unwrap(), time(next) + 2_000_000, "{row}");
        assert_eq!(row[1].as_u64().unwrap(), next as u64, "{row}");
        assert!(
            (row[3].as_f64().unwrap() - (closes[next] - 3.0)).abs() < 1e-9,
            "{row}"
        );
    }

    for settings in [
        json!({"fill_timing": "next_open", "latency": {"on": true, "value": 10}}),
        json!({"fill_timing": "next_open", "precision": "high"}),
    ] {
        let error = lab
            .dispatch(
                "run_backtest",
                &on_hand_market(
                    json!({"strategy": {"positions": [[time(10), 1]]}, "settings": settings}),
                ),
            )
            .unwrap_err();
        assert!(error.contains("next open"), "{settings}: {error}");
    }
}

#[test]
fn what_the_app_draws_an_agent_can_read() {
    let (lab, _source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let span = json!({"symbol": "BTCUSDT", "bars": "1h", "from": at(0), "to": at(2_500), "wait_seconds": 120});
    let with_span = |more: Value| {
        let mut args = span.clone();
        for (key, value) in more.as_object().unwrap() {
            args[key] = value.clone();
        }
        args
    };
    let ema = json!({"builtin": "ema-cross", "params": {"Size %": 25}});

    // The line beside the equity: a quarter of the capital held in the market, worked by hand.
    let run = lab
        .dispatch("run_backtest", &with_span(json!({"strategy": ema})))
        .unwrap();
    let bars = lab
        .dispatch(
            "get_bars",
            &json!({"symbol": "BTCUSDT", "bars": "1h", "from": at(0), "to": at(2_500), "limit": 3_000}),
        )
        .unwrap();
    let close_at = bars["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c == "close")
        .unwrap();
    let closes: Vec<f64> = bars["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[close_at].as_f64().unwrap())
        .collect();
    let equity = lab
        .dispatch(
            "get_equity",
            &json!({"run_id": run["run_id"], "limit": 3_000}),
        )
        .unwrap();
    let rows = equity["rows"].as_array().unwrap();
    assert_eq!(equity["columns"][3], "market", "{equity}");
    assert_eq!(rows.len(), closes.len());
    for (row, close) in rows.iter().zip(&closes) {
        let expected = 100_000.0 * (1.0 + 0.25 * (close / closes[0] - 1.0));
        assert!((row[3].as_f64().unwrap() - expected).abs() < 1e-6, "{row}");
    }

    // Random entries' charts: everything counted, and the backtest's figure within range.
    let drawn = lab
        .dispatch(
            "run_random_entries",
            &json!({"run_id": run["run_id"], "runs": 12, "wait_seconds": 120}),
        )
        .unwrap();
    for (chart, actual) in [
        ("return_chart", "actual_return"),
        ("fall_chart", "actual_fell"),
    ] {
        let chart = &drawn[chart];
        let counted: u64 = chart["counts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_u64().unwrap())
            .sum();
        assert_eq!(counted, 12, "{chart}");
        assert_eq!(chart["actual"], drawn[actual], "{chart}");
        let (from, to) = (
            chart["from"].as_f64().unwrap(),
            chart["to"].as_f64().unwrap(),
        );
        assert!(
            (from..=to).contains(&chart["actual"].as_f64().unwrap()),
            "{chart}"
        );
    }

    // Walk-forward's chart ends where its figures say, a start for each window tested.
    let walked = lab
        .dispatch(
            "run_walk_forward",
            &with_span(json!({
                "strategy": ema,
                "windows": 3,
                "variables": [{"name": "Fast", "min": 5, "max": 8, "step": 3}],
            })),
        )
        .unwrap();
    let chart = &walked["chart"];
    let last = |line: &str| {
        chart[line]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_f64()
            .unwrap()
    };
    let near = |a: f64, b: f64| assert!((a - b).abs() < 1e-3, "{a} against {b}: {walked}");
    near(
        last("unseen_percent"),
        walked["unseen_return"].as_f64().unwrap() * 100.0,
    );
    near(
        last("market_percent"),
        walked["buy_and_hold_return"].as_f64().unwrap() * 100.0,
    );
    assert_eq!(
        chart["window_starts"].as_array().unwrap().len(),
        3,
        "{chart}"
    );
    // Thin enough not to cost an agent much of its context.
    assert!(
        chart["unseen_percent"].as_array().unwrap().len() <= 202,
        "{chart}"
    );
    assert_eq!(
        chart["unseen_percent"].as_array().unwrap().len(),
        chart["market_percent"].as_array().unwrap().len(),
    );
}

#[test]
fn every_fan_and_heatmap_the_app_draws_an_agent_can_read() {
    let (lab, _source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let span = |more: Value| {
        let mut args = json!({
            "symbol": "BTCUSDT", "bars": "1h", "from": at(0), "to": at(2_500), "wait_seconds": 120,
            "strategy": {"builtin": "ema-cross"},
        });
        for (key, value) in more.as_object().unwrap() {
            args[key] = value.clone();
        }
        args
    };
    let run = lab.dispatch("run_backtest", &span(json!({}))).unwrap();
    assert_eq!(run["summary"]["fills_on"], "bars", "{run}");
    let next = lab
        .dispatch(
            "run_backtest",
            &span(json!({"settings": {"fill_timing": "next_open"}})),
        )
        .unwrap();
    assert_eq!(next["summary"]["fills_on"], "next_open", "{next}");

    let drawn = lab
        .dispatch(
            "run_monte_carlo",
            &json!({"run_id": run["run_id"], "runs": 30, "seed": 1, "wait_seconds": 120}),
        )
        .unwrap();
    let chart = &drawn["chart"];
    for line in [
        "median_line",
        "backtest_line",
        "best_line",
        "worst_line",
        "middle_line",
    ] {
        let points = chart[line].as_array().unwrap();
        assert!(points.len() > 1 && points.len() <= 102, "{line}: {chart}");
    }
    // The real line ends where the run did, and the extremes where the table says.
    let last = |line: &str| {
        chart[line]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_f64()
            .unwrap()
    };
    let capital = run["summary"]["final_equity"].as_f64().unwrap();
    assert!((last("backtest_line") - capital).abs() < 0.01, "{chart}");
    assert!(last("best_line") >= last("worst_line"), "{chart}");
    assert!(chart.get("lines").is_none(), "only when asked: {chart}");
    let every = lab
        .dispatch(
            "run_monte_carlo",
            &json!({"run_id": run["run_id"], "runs": 30, "seed": 1, "include_lines": true, "wait_seconds": 120}),
        )
        .unwrap();
    assert!(
        every["chart"]["lines"]
            .as_array()
            .is_some_and(|l| !l.is_empty()),
        "{every}"
    );

    let blocks = lab
        .dispatch(
            "run_random_blocks",
            &span(json!({"blocks": 6, "length": "week", "include_lines": true})),
        )
        .unwrap();
    assert!(
        blocks["chart"]["median_line"]
            .as_array()
            .is_some_and(|l| l.len() > 1),
        "{blocks}"
    );
    assert!(
        blocks["chart"]["lines"]
            .as_array()
            .is_some_and(|l| !l.is_empty()),
        "{blocks}"
    );

    // The whole grid, however few are listed: what the heatmap is drawn from.
    let found = lab
        .dispatch(
            "run_optimize",
            &span(json!({
                "limit": 1,
                "variables": [
                    {"name": "Fast", "min": 5, "max": 9, "step": 2},
                    {"name": "Slow", "min": 20, "max": 30, "step": 10},
                ],
            })),
        )
        .unwrap();
    let chart = &found["chart"];
    assert_eq!(found["values"].as_array().unwrap().len(), 1);
    assert_eq!(chart["across"]["name"], "Fast");
    assert_eq!(chart["across"]["values"], json!([5.0, 7.0, 9.0]));
    assert_eq!(chart["down"]["name"], "Slow");
    assert_eq!(chart["scores"].as_array().unwrap().len(), 6, "{chart}");
}

#[test]
fn the_docs_name_every_tool_and_what_each_screen_comes_from() {
    let docs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let overview = fs::read_to_string(docs.join("lab.md")).unwrap();
    for tool in tools() {
        assert!(
            overview.contains(&format!("`{}`", tool.name)),
            "`{}` isn't in docs/lab.md",
            tool.name
        );
    }
    // Every test the app's panel has is in the table of what's where, with every chart's field.
    let guide = fs::read_to_string(docs.join("lab-agents.md")).unwrap();
    for named in [
        "Monte Carlo",
        "Random blocks",
        "Optimize",
        "Walk-forward",
        "Random entries",
        "Calendar",
        "`get_equity`'s `market`",
        "`return_chart`",
        "`fall_chart`",
        "`fills_on`",
        "include_lines",
        "`gross_return`",
        "`benchmark`",
        "`export_state`",
        "`import_state`",
    ] {
        assert!(guide.contains(named), "{named} isn't in the agents' guide");
    }
}

#[test]
fn everything_moves_to_another_lab_in_one_file_and_no_keys_go_with_it() {
    // `lab()` puts an API key beside everything else.
    let (lab, dir) = lab();
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": ema(), "label": "moved"})),
        )
        .unwrap();
    lab.dispatch(
        "save_result",
        &json!({"name": "To move", "strategy": ema(), "run_id": run["run_id"], "settings": {"compound": true}}),
    )
    .unwrap();
    fs::write(dir.join("layout.ron"), "(layout)").unwrap();

    let file = dir.join("moved.json");
    let out = lab
        .dispatch("export_state", &json!({"path": file, "include_runs": true}))
        .unwrap();
    assert_eq!(out["results"], 1, "{out}");
    assert_eq!(out["runs"], 1, "{out}");
    let text = fs::read_to_string(&file).unwrap();
    assert!(!text.contains(SECRET), "a key went out");
    // It won't write over a file, as with every other export here.
    let again = lab
        .dispatch("export_state", &json!({"path": file}))
        .unwrap_err();
    assert!(again.contains("already exists"), "{again}");

    let (other, _other_dir) = self::lab();
    let done = other
        .dispatch("import_state", &json!({"path": file}))
        .unwrap();
    assert_eq!(done["results_added"], 1, "{done}");
    assert_eq!(done["runs_added"], 1, "{done}");
    assert_eq!(done["not_applied"], json!(["layout"]), "{done}");
    // The run reads back the same, the result's listed, and its settings came too.
    let back = other
        .dispatch("get_report", &json!({"run_id": run["run_id"]}))
        .unwrap();
    assert_eq!(back["summary"], run["summary"]);
    let results = other.dispatch("list_results", &json!({})).unwrap();
    assert_eq!(results["results"][0]["name"], "To move");
    assert_eq!(results["results"][0]["settings"]["compound"], true);
    let listed = other
        .dispatch("list_runs", &json!({"label": "moved"}))
        .unwrap();
    assert_eq!(listed["total"], 1, "{listed}");
    // Twice changes nothing.
    let twice = other
        .dispatch("import_state", &json!({"path": file}))
        .unwrap();
    assert_eq!(twice["results_added"], 0, "{twice}");
    assert_eq!(twice["results_skipped"], 1, "{twice}");

    // One chart's setup, for the app or another Lab.
    let chart = dir.join("chart.json");
    lab.dispatch(
        "export_state",
        &json!({"path": chart, "strategy": ema(), "settings": {"compound": true}, "name": "Mine"}),
    )
    .unwrap();
    let setup = other
        .dispatch("import_state", &json!({"path": chart}))
        .unwrap();
    assert_eq!(setup["chart"]["name"], "Mine", "{setup}");
    assert_eq!(setup["chart"]["strategy"], ema(), "{setup}");

    // Something else entirely says so.
    fs::write(dir.join("other.json"), "{}").unwrap();
    let error = other
        .dispatch("import_state", &json!({"path": dir.join("other.json")}))
        .unwrap_err();
    assert!(error.contains("isn't a Persephone"), "{error}");
}

#[test]
fn limits_stops_fees_and_impact_work_from_settings_and_report_what_they_did() {
    let closes: Vec<f64> = (0..120)
        .map(|h| 100.0 + 5.0 * (f64::from(h) / 4.0).sin())
        .collect();
    let (lab, times) = market_of(&closes);
    let list = json!([
        [times[10], 1],
        [times[40], 0],
        [times[60], -1],
        [times[90], 0]
    ]);
    let run = |settings: Value| {
        lab.dispatch(
            "run_backtest",
            &on_hand_market(json!({"strategy": {"positions": list}, "settings": settings})),
        )
        .unwrap()
    };
    // Bars here have no range to speak of, so only closes matter: a limit must be through at a close.
    let limited = run(json!({
        "commission": {"on": true, "value": {"rates": {"maker": 0.01, "taker": 0.05}}},
        "limit": {"on": true, "value": {"offset_percent": 0.5, "expires_bars": 5}},
    }));
    let summary = &limited["summary"];
    assert!(summary["limit_orders"].as_u64().unwrap() >= 4, "{summary}");
    assert_eq!(
        summary["limits_filled"].as_u64().unwrap() as f64
            / summary["limit_orders"].as_f64().unwrap(),
        summary["fill_rate"].as_f64().unwrap()
    );
    assert!(summary["maker_fees"].as_f64().unwrap() > 0.0, "{summary}");
    let fills = lab
        .dispatch("get_fills", &json!({"run_id": limited["run_id"]}))
        .unwrap();
    assert_eq!(fills["columns"][6], "maker");
    assert!(
        fills["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row[6] == true),
        "{fills}"
    );

    // A bad choice says so.
    let error = lab
        .dispatch(
            "run_backtest",
            &on_hand_market(json!({
                "strategy": {"positions": list},
                "settings": {
                    "fill_timing": "next_open",
                    "limit": {"on": true, "value": {"offset_percent": 0.5, "expires_bars": 5}},
                },
            })),
        )
        .unwrap_err();
    assert!(error.contains("limit orders"), "{error}");

    // Costs that only exist while held and only when ordered still come out of the gross.
    let impact = run(json!({"impact_percent": {"on": true, "value": 5}}));
    let plain = run(json!({"commission": {"on": true, "value": {"percent": 0}}}));
    assert!(
        impact["summary"]["net_return"].as_f64().unwrap()
            < impact["summary"]["gross_return"].as_f64().unwrap(),
        "{impact}"
    );
    assert_eq!(plain["summary"]["limit_orders"], 0);
    assert!(plain["summary"]["fill_rate"].is_null(), "{plain}");
    let borrowed = run(json!({"borrow_percent_per_year": {"on": true, "value": 50}}));
    assert!(
        borrowed["summary"]["borrow_paid"].as_f64().unwrap() > 0.0,
        "{borrowed}"
    );
}

#[test]
fn many_backtests_of_a_perpetual_at_once_all_finish() {
    // They all want its funding at the same moment, which once failed some with a file gone missing.
    let (lab, source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let runs: Vec<Value> = (5..37)
        .map(|fast| json!({"strategy": {"builtin": "ema-cross", "params": {"Fast": fast, "Slow": 60}}}))
        .collect();
    let batch = lab
        .dispatch(
            "run_backtests",
            &json!({
                "defaults": {"symbol": "BTCUSDT-PERP", "bars": "1h", "from": at(0), "to": at(500)},
                "runs": runs,
                "wait_seconds": 120,
            }),
        )
        .unwrap();
    assert_eq!(batch["failed"], 0, "{batch}");
    assert_eq!(
        source.funding_requests.load(Ordering::Relaxed),
        1,
        "fetched once"
    );

    // The rates themselves, as an agent checking it needs them.
    let rates = lab
        .dispatch(
            "get_funding",
            &json!({"symbol": "BTCUSDT-PERP", "from": at(0), "to": at(100), "limit": 3}),
        )
        .unwrap();
    assert_eq!(rates["columns"][2], "rate");
    assert_eq!(rates["rows"][0][2], 0.0001, "{rates}");
    let spot = lab
        .dispatch("get_funding", &json!({"symbol": "BTCUSDT"}))
        .unwrap_err();
    assert!(spot.contains("BTCUSDT-PERP"), "{spot}");

    // The market's return, and what it left after funding.
    let run = lab
        .dispatch(
            "run_backtest",
            &json!({
                "symbol": "BTCUSDT-PERP", "bars": "1h", "from": at(0), "to": at(500),
                "strategy": {"builtin": "ema-cross"}, "wait_seconds": 120,
            }),
        )
        .unwrap();
    let held = &run["summary"]["benchmark"];
    assert!(
        held["return_after_funding"].as_f64().unwrap() < held["return"].as_f64().unwrap(),
        "{held}"
    );
}

#[test]
fn a_perpetual_has_its_own_market_fees_and_pays_funding() {
    let (lab, source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let args = |symbol: &str| {
        json!({
            "symbol": symbol, "bars": "1h", "from": at(0), "to": at(200), "wait_seconds": 120,
            "strategy": {"positions": [[at(10), 1]]},
        })
    };
    let perp = lab.dispatch("run_backtest", &args("BTCUSDT-PERP")).unwrap();
    let summary = &perp["summary"];
    // FakeBinance pays 0.01% every 8 hours, and a long pays that.
    assert!(summary["funding_paid"].as_f64().unwrap() > 0.0, "{summary}");
    assert!(
        summary["total_costs_paid"].as_f64().unwrap() > summary["funding_paid"].as_f64().unwrap()
    );
    let spot = lab.dispatch("run_backtest", &args("BTCUSDT")).unwrap();
    assert_eq!(spot["summary"]["funding_paid"], json!(0.0));
    // The perp's taker fee is 0.05% against spot's 0.1%.
    let fee = |run: &Value| run["summary"]["fees"].as_f64().unwrap();
    assert!(
        (fee(&spot) / fee(&perp) - 2.0).abs() < 0.2,
        "{} {}",
        fee(&spot),
        fee(&perp)
    );
    // Rates are downloaded once.
    let asked = source.funding_requests.load(Ordering::Relaxed);
    lab.dispatch("run_backtest", &args("BTCUSDT-PERP")).unwrap();
    assert_eq!(source.funding_requests.load(Ordering::Relaxed), asked);
}

#[test]
fn every_limit_order_can_be_read_with_what_followed_and_a_queue_can_be_required() {
    // A wave with real range, so limits are reached by some bars and not others.
    let (lab, _source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let cycle: Vec<Value> = (0..200)
        .map(|i| json!([at(10 + i * 3), if i % 2 == 0 { 1 } else { 0 }]))
        .collect();
    let args = |through: f64| {
        json!({
            "symbol": "BTCUSDT", "bars": "1h", "from": at(0), "wait_seconds": 120,
            "strategy": {"positions": cycle},
            "settings": {"limit": {"on": true, "value": {
                "offset_percent": 0.02, "expires_bars": 2, "through_percent": through,
            }}},
        })
    };
    let free = lab.dispatch("run_backtest", &args(0.0)).unwrap();
    let queued = lab.dispatch("run_backtest", &args(0.3)).unwrap();
    let filled = |run: &Value| run["summary"]["limits_filled"].as_f64().unwrap();
    assert!(
        filled(&free) > filled(&queued),
        "{} {}",
        filled(&free),
        filled(&queued)
    );

    let page = lab
        .dispatch(
            "get_limit_orders",
            &json!({"run_id": free["run_id"], "limit": 1000}),
        )
        .unwrap();
    assert_eq!(page["columns"][5], "outcome");
    let rows = page["rows"].as_array().unwrap();
    assert_eq!(
        rows.len() as u64,
        free["summary"]["limit_orders"].as_u64().unwrap()
    );
    let summary = &free["summary"]["limit_summary"];
    assert_eq!(
        rows.iter().filter(|row| row[5] == "filled").count() as u64,
        summary["filled"].as_u64().unwrap(),
        "{summary}"
    );
    // Every fill reached past its price, and every miss didn't get there.
    for row in rows {
        let reached = row[8].as_f64().unwrap_or(f64::NAN);
        if row[5] == "filled" {
            assert!(reached > 0.0, "{row}");
        } else if row[5] == "lapsed" {
            assert!(reached <= 0.0, "{row}");
        }
    }
    let only = lab
        .dispatch(
            "get_limit_orders",
            &json!({"run_id": free["run_id"], "outcome": "lapsed", "limit": 1000}),
        )
        .unwrap();
    assert!(
        only["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row[5] == "lapsed"),
        "{only}"
    );
    assert!(
        free["summary"]["limit_summary"]["filled_move_1"].is_number(),
        "{summary}"
    );

    // None sent, none to say.
    let market = lab
        .dispatch(
            "run_backtest",
            &json!({
                "symbol": "BTCUSDT", "bars": "1h", "from": at(0), "wait_seconds": 120,
                "strategy": {"positions": cycle},
            }),
        )
        .unwrap();
    assert!(market["summary"]["limit_summary"].is_null(), "{market}");
}

#[test]
fn a_look_back_longer_than_the_warm_up_says_so_even_when_it_trades() {
    let (lab, _source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let run_with = |long: Value| {
        lab.dispatch(
            "run_backtest",
            &json!({
                "symbol": "BTCUSDT", "bars": "1h", "from": at(0), "to": at(1_000), "wait_seconds": 120,
                "strategy": {"logic": {"name": "Either", "long": long, "flat": {"ge": ["bars_held", 3]}}},
            }),
        )
        .unwrap()
    };
    let short = json!({"crosses_above": [{"ema": {"length": 5}}, {"ema": {"length": 15}}]});
    let run = run_with(json!({"or": [
        short.clone(),
        {"gt": ["close", {"sma": {"length": 800}}]},
    ]}));
    assert!(run["summary"]["fills"].as_u64().unwrap() > 0, "{run}");
    let warnings = run["summary"]["warnings"].to_string();
    assert!(warnings.contains("about 800 bars"), "{warnings}");

    // One the warm-up covers says nothing.
    let run = run_with(short);
    assert_eq!(run["summary"]["warnings"], json!([]), "{run}");
}

#[test]
fn the_batch_table_has_each_runs_years() {
    let (lab, _source) = binance_lab();
    let at = |hour: u64| START + hour * HOUR;
    let batch = lab
        .dispatch(
            "run_backtests",
            &json!({
                "defaults": {"symbol": "BTCUSDT", "bars": "1h", "from": at(0)},
                "wait_seconds": 120,
                "runs": [{"strategy": {"builtin": "ema-cross"}}],
            }),
        )
        .unwrap();
    let columns = batch["columns"].as_array().unwrap();
    let column = columns.iter().position(|c| c == "by_year").unwrap();
    let row = &batch["rows"][0];
    let run = lab
        .dispatch("get_report", &json!({"run_id": row[1]}))
        .unwrap();
    assert_eq!(row[column], run["summary"]["by_year"], "{batch}");
    assert!(
        row[column].as_object().is_some_and(|y| !y.is_empty()),
        "{batch}"
    );
    // Warnings come as their words, not how many there are.
    let warnings = columns.iter().position(|c| c == "warnings").unwrap();
    assert!(row[warnings].is_array(), "{batch}");
}

#[test]
fn pages_and_exports_carry_on_past_a_hole_in_the_market() {
    let source = Arc::new(FakeBinance {
        funding_requests: AtomicUsize::new(0),
        requests: AtomicUsize::new(0),
        hole: Some((400, 700)),
    });
    let dir = std::env::temp_dir().join(format!("persephone-lab-hole-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let lab = Lab::with_source(Paths::under(&dir), source).unwrap();
    // A page ending in the hole, and pages smaller than it.
    let mut args = json!({"symbol": "BTCUSDT", "bars": "1h", "from": START, "limit": 150});
    let mut got = Vec::new();
    loop {
        let page = lab.dispatch("get_bars", &args).unwrap();
        got.extend(
            page["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row[0].as_u64().unwrap()),
        );
        match page["next_cursor"].as_str() {
            Some(cursor) => args["cursor"] = json!(cursor),
            None => break,
        }
    }
    let expected: Vec<u64> = (0..HOURS)
        .filter(|hour| !(400..700).contains(hour))
        .map(|hour| START + hour * HOUR)
        .collect();
    assert_eq!(got, expected);

    let path = dir.join("all.csv");
    let export = lab
        .dispatch(
            "export_data",
            &json!({
                "symbol": "BTCUSDT", "bars": "1h", "from": START, "format": "csv",
                "path": path, "wait_seconds": 60,
            }),
        )
        .unwrap();
    assert_eq!(export["rows"], expected.len(), "{export}");
    assert_eq!(export["columns"][0], "ts");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_strategy_trading_like_chance_looks_like_chance() {
    // A smooth market, so entries owe nothing to the strategy: a list of its own holds at set times.
    let (lab, _dir) = lab();
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": ema(), "wait_seconds": 120})),
        )
        .unwrap();
    let args = json!({"run_id": run["run_id"], "runs": 30, "seed": 3, "wait_seconds": 120});
    let answer = lab.dispatch("run_random_entries", &args).unwrap();
    assert_eq!(answer["runs"], 30);
    assert_eq!(answer["trades"], run["summary"]["trades"]);
    assert_eq!(answer["actual_return"], run["summary"]["net_return"]);
    let share = answer["share_matching_return"].as_f64().unwrap();
    assert!((0.0..=1.0).contains(&share), "{answer}");
    assert!(answer["low"].as_f64() <= answer["median"].as_f64());
    assert!(answer["median"].as_f64() <= answer["high"].as_f64());
    assert!(answer["low"].as_f64() < answer["high"].as_f64(), "{answer}");
    // The same seed repeats exactly.
    let again = lab.dispatch("run_random_entries", &args).unwrap();
    assert_eq!(again["median"], answer["median"]);
    assert_eq!(again["share_matching_fall"], answer["share_matching_fall"]);

    // No trades, nothing to place, and a word on why there may be none.
    let idle = lab
        .dispatch(
            "run_backtest",
            &with(json!({
                "wait_seconds": 120,
                "strategy": {"logic": {"long": {"gt": ["close", {"sma": {"length": 10000}}]}}},
            })),
        )
        .unwrap();
    assert!(
        idle["summary"]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("look-back"),
        "{idle}"
    );
    let error = lab
        .dispatch(
            "run_random_entries",
            &json!({"run_id": idle["run_id"], "wait_seconds": 60}),
        )
        .unwrap_err();
    assert!(error.contains("no trades"), "{error}");
}

#[test]
fn years_add_up_and_long_lists_can_come_from_files() {
    let (lab, dir) = lab();
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": ema(), "label": "one", "wait_seconds": 120})),
        )
        .unwrap();
    let summary = &run["summary"];
    let years = summary["by_year"].as_object().unwrap();
    assert!(!years.is_empty(), "{summary}");
    let sum: f64 = years.values().map(|year| year.as_f64().unwrap()).sum();
    assert!(
        (sum - summary["net_return"].as_f64().unwrap()).abs() < 1e-9,
        "{summary}"
    );

    let runs = dir.join("runs.json");
    fs::write(
        &runs,
        json!([{"strategy": ema(), "label": "filed"}]).to_string(),
    )
    .unwrap();
    let batch = lab
        .dispatch(
            "run_backtests",
            &json!({"runs": runs, "defaults": market(), "wait_seconds": 120}),
        )
        .unwrap();
    assert_eq!(batch["succeeded"], 1, "{batch}");
    let label = batch["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c == "label")
        .unwrap();
    assert_eq!(batch["rows"][0][label], "filed");

    let bars = lab
        .dispatch("get_bars", &with(json!({"from": "2024-01-02", "limit": 2})))
        .unwrap();
    let list = dir.join("list.json");
    fs::write(
        &list,
        json!([[bars["rows"][0][0], 1], [bars["rows"][1][0], 0]]).to_string(),
    )
    .unwrap();
    let listed = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": {"positions": list}, "wait_seconds": 120})),
        )
        .unwrap();
    assert_eq!(listed["summary"]["fills"], 2, "{listed}");
}

#[test]
fn an_answer_too_big_for_an_agent_goes_to_a_file() {
    let (lab, _dir) = lab();
    let small = json!({"rows": [1, 2, 3]});
    assert_eq!(lab.fit_for_agent(&small), small.to_string());

    let big = json!({"rows": vec!["x".repeat(1_000); 200]});
    let answer: Value = serde_json::from_str(&lab.fit_for_agent(&big)).unwrap();
    assert_eq!(answer["too_large"], true, "{answer}");
    let saved = fs::read_to_string(answer["path"].as_str().unwrap()).unwrap();
    assert_eq!(saved, big.to_string());
    assert!(answer["hint"].as_str().unwrap().contains("limit"));
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

/// Two markets made by hand, the second missing a bar.
fn two_hand_markets() -> Arc<Lab> {
    let (lab, dir) = lab();
    for (symbol, start, step, skip) in [
        ("AAAUSD", 100.0, 1.0, None),
        ("BBBUSD", 50.0, -0.1, Some(10)),
    ] {
        let file = dir.join(format!("{symbol}.csv"));
        let mut text = String::from("time,open,high,low,close,volume\n");
        for i in 0..48 {
            if skip == Some(i) {
                continue;
            }
            let close: f64 = start + step * f64::from(i);
            text += &format!(
                "2024-01-{:02} {:02}:00:00,{close},{close},{close},{close},1000\n",
                1 + i / 24,
                i % 24
            );
        }
        fs::write(&file, text).unwrap();
        lab.dispatch(
            "import_data",
            &json!({"files": [file], "symbol": symbol, "venue": "HAND", "wait_seconds": 60}),
        )
        .unwrap();
    }
    lab
}

fn pair(more: Value) -> Value {
    let mut args = json!({
        "legs": [
            {"symbol": "AAAUSD", "venue": "HAND", "weight": 0.5},
            {"symbol": "BBBUSD", "venue": "HAND", "weight": -0.5},
        ],
        "bars": "1h",
        "wait_seconds": 120,
    });
    for (key, value) in more.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}

#[test]
fn a_portfolio_runs_from_tools_and_every_part_of_it_can_be_read() {
    let lab = two_hand_markets();
    let run = lab.dispatch("run_portfolio", &pair(json!({}))).unwrap();
    let summary = &run["summary"];
    assert_eq!(summary["kind"], "portfolio");
    assert_eq!(summary["bars"], 48);
    assert!(summary["net_return"].as_f64().unwrap() > 0.0, "{summary}");
    assert!(
        summary["warnings"][0]
            .as_str()
            .unwrap()
            .contains("BBBUSD had no bar"),
        "{summary}"
    );
    let id = run["run_id"].as_str().unwrap();

    let whole = lab
        .dispatch("get_portfolio", &json!({"run_id": id}))
        .unwrap();
    assert_eq!(whole["legs"][1]["symbol"], "BBBUSD");
    assert_eq!(whole["legs"][1]["missing_bars"], 1);
    assert_eq!(whole["correlation"].as_array().unwrap().len(), 2);

    let series = lab
        .dispatch("get_portfolio_series", &json!({"run_id": id, "limit": 5}))
        .unwrap();
    assert_eq!(series["total"], 48);
    let columns = series["columns"].as_array().unwrap();
    assert!(columns.contains(&json!("BBBUSD_made")), "{columns:?}");
    assert_eq!(series["rows"][0].as_array().unwrap().len(), columns.len());

    let fills = lab
        .dispatch(
            "get_portfolio_fills",
            &json!({"run_id": id, "symbol": "bbbusd"}),
        )
        .unwrap();
    assert_eq!(fills["rows"][0][2], "sell");

    // One trade a leg, long A and short B, both still open: with no funding, borrowing or
    // interest here, they come to what the account made.
    let trades = lab
        .dispatch("get_portfolio_trades", &json!({"run_id": id}))
        .unwrap();
    assert_eq!(trades["total"], 2, "{trades}");
    let markets: Vec<&Value> = trades["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row[1])
        .collect();
    assert!(markets.contains(&&json!("AAAUSD")) && markets.contains(&&json!("BBBUSD")));
    let made = summary["final_equity"].as_f64().unwrap() - 100_000.0;
    let profit = trades["profit"].as_f64().unwrap();
    assert!((profit - made).abs() < 1e-6, "{profit} against {made}");
    let shorts = lab
        .dispatch(
            "get_portfolio_trades",
            &json!({"run_id": id, "symbol": "bbbusd"}),
        )
        .unwrap();
    assert_eq!(shorts["total"], 1);
    assert_eq!(shorts["rows"][0][2], "short");
    assert!(
        lab.dispatch(
            "get_portfolio_trades",
            &json!({"run_id": id, "symbol": "XXX"})
        )
        .is_err()
    );

    // A backtest's readers say it isn't one of theirs, and the other way about.
    let wrong = lab
        .dispatch(
            "get_portfolio_fills",
            &json!({"run_id": id, "symbol": "XXX"}),
        )
        .unwrap_err();
    assert!(wrong.contains("XXX"), "{wrong}");
    let listed = lab.dispatch("list_runs", &json!({})).unwrap();
    assert!(listed.to_string().contains(id));
    let backtest_reader = lab
        .dispatch("get_fills", &json!({"run_id": id}))
        .unwrap_err();
    assert!(
        backtest_reader.contains("get_portfolio"),
        "{backtest_reader}"
    );
}

#[test]
fn a_weight_table_and_costs_work_from_tools_and_a_sweep_ranks_them() {
    let lab = two_hand_markets();
    let table = json!([
        ["2024-01-01", {"AAAUSD": 0.5, "BBBUSD": -0.5}],
        ["2024-01-01 12:00", {"AAAUSD": 0.25}],
    ]);
    let run = lab
        .dispatch(
            "run_portfolio",
            &pair(json!({
                "weights": table,
                "settings": {
                    "commission": {"on": true, "value": {"percent": 0.1}},
                    "max_participation_percent": {"on": true, "value": 50},
                    "range_slippage_percent": {"on": true, "value": 1},
                },
            })),
        )
        .unwrap();
    let summary = &run["summary"];
    assert!(
        summary["total_costs_paid"].as_f64().unwrap() > 0.0,
        "{summary}"
    );
    assert!(
        summary["net_return"].as_f64().unwrap() < summary["gross_return"].as_f64().unwrap(),
        "{summary}"
    );

    let sweep = lab
        .dispatch(
            "run_portfolios",
            &json!({
                "runs": [
                    pair(json!({"label": "half"})),
                    {"legs": [{"symbol": "AAAUSD", "venue": "HAND", "weight": 0.1}], "bars": "1h", "label": "little"},
                    {"legs": [{"symbol": "NOPE", "venue": "HAND", "weight": 0.1}], "bars": "1h"},
                ],
                "wait_seconds": 120,
            }),
        )
        .unwrap();
    assert_eq!(sweep["succeeded"], 2, "{sweep}");
    assert_eq!(sweep["failed"], 1, "{sweep}");
    assert_eq!(sweep["rows"][0][2], "half", "{sweep}");
}

#[test]
fn what_a_portfolio_cant_take_is_said_plainly() {
    let lab = two_hand_markets();
    let wrong = |args: Value| lab.dispatch("run_portfolio", &args).unwrap_err();
    assert!(wrong(json!({"legs": [], "wait_seconds": 60})).contains("`legs`"));
    assert!(wrong(pair(json!({"rebalance": {"evry_bars": 2}}))).contains("evry_bars"));
    let misspelt =
        json!({"legs": [{"symbol": "AAAUSD", "venue": "HAND", "wieght": 1}], "wait_seconds": 60});
    assert!(wrong(misspelt).contains("wieght"));
    let no_weight = json!({"legs": [{"symbol": "AAAUSD", "venue": "HAND"}], "wait_seconds": 60});
    assert!(wrong(no_weight).contains("needs a weight"));
    let stops = pair(json!({"settings": {"stop_loss_percent": {"on": true, "value": 1}}}));
    assert!(wrong(stops).contains("at the market"));
}

#[test]
fn a_setup_in_settings_is_accepted_and_the_new_settings_are_listed() {
    let lab = two_hand_markets();
    let run = lab
        .dispatch(
            "run_portfolio",
            &pair(json!({
                "settings": {"portfolio": {"legs": [{"symbol": "AAAUSD", "weight": 1.0}]}},
            })),
        )
        .unwrap();
    assert!(run["run_id"].is_string());
    let defaults = lab.dispatch("get_settings", &json!({})).unwrap();
    for name in [
        "max_participation_percent",
        "range_slippage_percent",
        "latency_jitter",
        "latency_seed",
        "portfolio",
    ] {
        assert!(defaults["settings"].get(name).is_some(), "{name}");
    }
}

#[test]
fn one_market_as_a_portfolio_reads_as_its_backtest_does() {
    let (lab, _dir) = lab();
    let settings = json!({"commission": {"on": true, "value": {"percent": 0.1}}});
    let single = backtest_with(&lab, &settings);
    let portfolio = lab
        .dispatch(
            "run_portfolio",
            &json!({
                "legs": [{"symbol": "BTCUSDT", "venue": "TEST", "weight": 1.0, "strategy": ema()}],
                "bars": "1h",
                "settings": settings,
                "wait_seconds": 120,
            }),
        )
        .unwrap();
    let (a, b) = (&single["summary"], &portfolio["summary"]);
    for figure in ["first_bar", "last_bar", "bars"] {
        assert_eq!(a[figure], b[figure], "{figure}");
    }
    for figure in [
        "net_return",
        "max_drawdown",
        "sharpe",
        "gross_return",
        "total_costs_paid",
    ] {
        let (x, y) = (a[figure].as_f64().unwrap(), b[figure].as_f64().unwrap());
        assert!((x - y).abs() < 1e-9, "{figure}: {x} and {y}");
    }
    let (x, y) = (&a["by_year"]["2024"], &b["by_year"]["2024"]);
    assert!(
        (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 1e-9,
        "{x} and {y}"
    );
}

fn backtest_with(lab: &Arc<Lab>, settings: &Value) -> Value {
    lab.dispatch(
        "run_backtest",
        &with(json!({"strategy": ema(), "settings": settings, "wait_seconds": 120})),
    )
    .unwrap()
}

#[test]
fn a_portfolio_can_fill_at_the_next_open_as_a_backtest_does() {
    let (lab, _dir) = lab();
    let settings = json!({"fill_timing": "next_open"});
    let single = backtest_with(&lab, &settings);
    let portfolio = lab
        .dispatch(
            "run_portfolio",
            &json!({
                "legs": [{"symbol": "BTCUSDT", "venue": "TEST", "weight": 1.0, "strategy": ema()}],
                "bars": "1h",
                "settings": settings,
                "wait_seconds": 120,
            }),
        )
        .unwrap();
    assert_eq!(portfolio["summary"]["fills_on"], "next_open");
    assert_eq!(single["summary"]["fills_on"], "next_open");
    let (x, y) = (
        single["summary"]["net_return"].as_f64().unwrap(),
        portfolio["summary"]["net_return"].as_f64().unwrap(),
    );
    assert!((x - y).abs() < 1e-9, "{x} and {y}");
}

#[test]
fn weight_rows_that_never_apply_are_said() {
    let lab = two_hand_markets();
    let run = lab
        .dispatch(
            "run_portfolio",
            &pair(json!({"weights": [["2030-01-01", {"AAAUSD": 0.5}]]})),
        )
        .unwrap();
    let warnings = run["summary"]["warnings"].to_string();
    assert!(warnings.contains("after the last bar"), "{warnings}");
}

#[test]
fn a_list_in_a_portfolio_trades_when_it_does_in_a_backtest() {
    let (lab, _dir) = lab();
    let bars = lab
        .dispatch("get_bars", &with(json!({"limit": 40})))
        .unwrap();
    let time = |at: usize| bars["rows"][at][0].clone();
    let list = json!([[time(10), 1], [time(20), -1], [time(30), 0]]);
    let single = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": {"positions": list}, "wait_seconds": 120})),
        )
        .unwrap();
    let portfolio = lab
        .dispatch(
            "run_portfolio",
            &json!({
                "legs": [{"symbol": "BTCUSDT", "venue": "TEST", "weight": 1.0, "strategy": {"positions": list}}],
                "bars": "1h",
                "wait_seconds": 120,
            }),
        )
        .unwrap();
    let id = portfolio["run_id"].as_str().unwrap();
    let theirs = lab
        .dispatch("get_fills", &json!({"run_id": single["run_id"]}))
        .unwrap();
    let ours = lab
        .dispatch(
            "get_portfolio_fills",
            &json!({"run_id": id, "symbol": "BTCUSDT"}),
        )
        .unwrap();
    assert_eq!(theirs["rows"].as_array().unwrap().len(), 3, "{theirs}");
    for (a, b) in theirs["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(ours["rows"].as_array().unwrap())
    {
        assert_eq!(a[0], b[0]);
        assert_eq!(a[3], b[3]);
    }
}

#[test]
fn a_longer_warm_up_loads_more_bars_before_the_span() {
    let (lab, _dir) = lab();
    let bars = lab
        .dispatch("get_bars", &with(json!({"limit": 200})))
        .unwrap();
    let from = bars["rows"][100][0].clone();
    let figures = |warm_up: u64| {
        lab.dispatch(
            "run_portfolio",
            &json!({
                "legs": [{"symbol": "BTCUSDT", "venue": "TEST", "weight": 0.5}],
                "bars": "1h",
                "from": from,
                "settings": {"warm_up_bars": warm_up},
                "wait_seconds": 120,
            }),
        )
        .unwrap()
    };
    let few = figures(1);
    let many = figures(24);
    let rows = |run: &Value| run["run_id"].as_str().unwrap().to_owned();
    let count = |id: String| {
        lab.load_run_for_test(&id).unwrap()["times"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(count(rows(&many)), count(rows(&few)) + 23);
    assert_eq!(few["summary"]["bars"], many["summary"]["bars"]);
}

#[test]
fn a_run_saved_outside_lab_is_read_by_lab_and_lab_s_by_anyone() {
    let lab = two_hand_markets();
    let run = lab.dispatch("run_portfolio", &pair(json!({}))).unwrap();
    let id = run["run_id"].as_str().unwrap();
    let dir = lab.paths.lab().join("runs");
    let back = persephone_research::portfolio::load_saved(&dir, id).unwrap();
    assert_eq!(back.report.legs[1].missing_bars, 1);
    assert_eq!(
        back.figures.net_return,
        run["summary"]["net_return"].as_f64().unwrap()
    );
    assert!(
        persephone_research::portfolio::list_saved(&dir)
            .iter()
            .any(|saved| saved.id == id)
    );

    // As the app writes one.
    let (summary, whole) = persephone_research::portfolio::written(
        &back,
        "run-from-the-app",
        1,
        &json!({"label": "App"}),
    );
    persephone_research::portfolio::save(&dir, &whole, &summary, &json!({"label": "App"})).unwrap();
    let read = lab
        .dispatch("get_portfolio", &json!({"run_id": "run-from-the-app"}))
        .unwrap();
    assert_eq!(read["summary"]["label"], "App");
    assert_eq!(read["legs"][1]["symbol"], "BBBUSD");
}

#[test]
fn no_costs_takes_off_everything_and_excess_and_by_time_add_up() {
    let (lab, _dir) = lab();
    let run = |settings: Value| {
        lab.dispatch(
            "run_backtest",
            &with(json!({"strategy": ema(), "settings": settings, "wait_seconds": 120})),
        )
        .unwrap()
    };
    let costly = json!({
        "commission": {"on": true, "value": {"percent": 0.1}},
        "slippage_percent": {"on": true, "value": 0.05},
        "range_slippage_percent": {"on": true, "value": 1},
    });
    let paying = run(costly.clone());
    let mut free = costly;
    free["no_costs"] = json!(true);
    let free = run(free);
    let plain = run(json!({"commission": {"on": true, "value": {"percent": 0}}}));
    let net = |run: &Value| run["summary"]["net_return"].as_f64().unwrap();
    assert!(net(&paying) < net(&free));
    assert_eq!(free["summary"]["fees"].as_f64().unwrap(), 0.0);
    assert!((net(&free) - net(&plain)).abs() < 1e-9);

    let summary = &paying["summary"];
    let excess = summary["excess_return"].as_f64().unwrap();
    let market = summary["benchmark"]["return_after_funding"]
        .as_f64()
        .unwrap();
    assert!((excess - (net(&paying) - market)).abs() < 1e-9);
    let made: f64 =
        summary["average_trade"].as_f64().unwrap() * summary["trades"].as_f64().unwrap();
    for times in ["by_hour", "by_weekday"] {
        let sum: f64 = summary[times]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .sum();
        assert!((sum - made).abs() < 1e-6, "{times}: {sum} and {made}");
    }
    assert!(
        lab.dispatch(
            "run_backtests",
            &json!({"runs": [with(json!({"strategy": ema()}))], "wait_seconds": 120})
        )
        .unwrap()["columns"]
            .to_string()
            .contains("excess_return")
    );
}

#[test]
fn a_portfolio_says_what_holding_its_weights_would_have_done() {
    let lab = two_hand_markets();
    let run = lab
        .dispatch(
            "run_portfolio",
            &pair(json!({"rebalance": {"every_bars": 1}})),
        )
        .unwrap();
    let summary = &run["summary"];
    // Long A from 100 to 147, short B from 50 to 45.3, half each, with no funding here.
    let held = summary["weights_held_return"].as_f64().unwrap();
    let expected = 0.5 * (147.0 / 100.0 - 1.0) - 0.5 * (45.3 / 50.0 - 1.0);
    assert!((held - expected).abs() < 1e-3, "{held} and {expected}");
    assert_eq!(
        summary["benchmark_after_funding"],
        summary["benchmark_return"]
    );
    assert_eq!(summary["by_hour"].as_array().unwrap().len(), 24);

    let by_strategy = lab
        .dispatch(
            "run_portfolio",
            &json!({"legs": [{"symbol": "AAAUSD", "venue": "HAND", "weight": 1.0, "strategy": ema()}], "bars": "1h", "wait_seconds": 120}),
        )
        .unwrap();
    assert!(by_strategy["summary"]["weights_held_return"].is_null());
}

#[test]
fn nothing_is_said_to_be_downloading_once_it_is_done() {
    let (lab, _source) = binance_lab();
    lab.dispatch(
        "get_bars",
        &json!({"symbol": "BTCUSDT", "bars": "1h", "from": "2024-02-01", "to": "2024-02-03"}),
    )
    .unwrap();
    assert_eq!(lab.cache.fetching(), None);
}

#[test]
fn a_run_of_either_kind_can_be_shifted_and_a_portfolio_drawn_and_walked() {
    let (lab, _dir) = lab();
    let single = backtest(&lab, &ema());
    let moved = lab
        .dispatch(
            "run_shifted",
            &json!({"run_id": single["run_id"], "runs": 8, "wait_seconds": 120}),
        )
        .unwrap();
    assert_eq!(moved["runs"], 8, "{moved}");
    assert!(moved["share_matching_return"].as_f64().unwrap() <= 1.0);
    assert_eq!(
        moved["share_matching_adjusted"], moved["share_matching_return"],
        "one variant by default"
    );
    let picked = lab
        .dispatch(
            "run_shifted",
            &json!({"run_id": single["run_id"], "runs": 8, "variants_tried": 11, "wait_seconds": 120}),
        )
        .unwrap();
    let share = picked["share_matching_return"].as_f64().unwrap();
    let adjusted = picked["share_matching_adjusted"].as_f64().unwrap();
    assert!(
        (adjusted - (1.0 - (1.0 - share).powi(11))).abs() < 1e-12,
        "{picked}"
    );
    assert_eq!(picked["variants_tried"], 11);
    let actual = moved["actual_return"].as_f64().unwrap();
    assert!((actual - single["summary"]["net_return"].as_f64().unwrap()).abs() < 1e-9);

    let lab = two_hand_markets();
    let run = lab.dispatch("run_portfolio", &pair(json!({}))).unwrap();
    let id = run["run_id"].clone();
    let moved = lab
        .dispatch(
            "run_shifted",
            &json!({"run_id": id, "runs": 6, "wait_seconds": 120}),
        )
        .unwrap();
    assert_eq!(moved["runs"], 6, "{moved}");
    assert!(moved["return_chart"]["counts"].is_array());

    let drawn = lab
        .dispatch(
            "run_portfolio_monte_carlo",
            &json!({"run_id": id, "runs": 50, "block_bars": 5, "seed": 1, "wait_seconds": 60}),
        )
        .unwrap();
    assert_eq!(drawn["runs"], 50);
    assert_eq!(drawn["block_bars"], 5);
    assert!(drawn["low_line"].as_array().unwrap().len() > 1);
    assert!(drawn["lines"].is_null());

    let legs = |weight: f64| json!([{"symbol": "AAAUSD", "venue": "HAND", "weight": weight}]);
    let walked = lab
        .dispatch(
            "run_portfolio_walk_forward",
            &json!({
                "candidates": [
                    {"legs": legs(0.5), "label": "long"},
                    {"legs": legs(-0.5), "label": "short"},
                ],
                "defaults": {"bars": "1h"},
                "windows": 2,
                "train_ratio": 1,
                "wait_seconds": 120,
            }),
        )
        .unwrap();
    // AAA only rises, so long's what's picked.
    assert_eq!(walked["windows"][0]["label"], "long", "{walked}");
    assert_eq!(walked["profitable_windows"], 2);

    // A literal `{}` table stays flat, and beats a candidate that loses.
    let flat_or_short = lab
        .dispatch(
            "run_portfolio_walk_forward",
            &json!({
                "candidates": [
                    {"legs": legs(-0.5), "label": "short"},
                    {"legs": legs(0.0), "weights": {}, "label": "flat"},
                ],
                "defaults": {"bars": "1h"},
                "windows": 2,
                "train_ratio": 1,
                "wait_seconds": 120,
            }),
        )
        .unwrap();
    for window in flat_or_short["windows"].as_array().unwrap() {
        assert_eq!(window["label"], "flat", "{flat_or_short}");
    }
    assert!(flat_or_short["unseen_return"].as_f64().unwrap().abs() < 1e-12);
    // A candidate that's wrong says which it is.
    let wrong = lab
        .dispatch(
            "run_portfolio_walk_forward",
            &json!({
                "candidates": [{"legs": legs(0.5)}, {"legs": legs(0.5), "weights": 3}],
                "defaults": {"bars": "1h"},
                "wait_seconds": 60,
            }),
        )
        .unwrap_err();
    assert!(wrong.contains("item 1"), "{wrong}");

    let wrong = lab
        .dispatch(
            "run_portfolio_walk_forward",
            &json!({"candidates": [], "wait_seconds": 60}),
        )
        .unwrap_err();
    assert!(wrong.contains("candidates"), "{wrong}");
}

#[test]
fn decisions_and_a_trailing_stop_work_from_settings() {
    let (lab, _dir) = lab();
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({
                "strategy": ema(),
                "settings": {
                    "decision_log": true,
                    "trailing_stop_percent": {"on": true, "value": 1},
                },
                "wait_seconds": 120,
            })),
        )
        .unwrap();
    let id = run["run_id"].clone();
    let page = lab
        .dispatch("get_decisions", &json!({"run_id": id, "limit": 3}))
        .unwrap();
    assert_eq!(page["rows"].as_array().unwrap().len(), 3);
    assert!(
        page["reasons"]["nothing asked"].as_u64().is_some(),
        "{page}"
    );
    let sent = lab
        .dispatch(
            "get_decisions",
            &json!({"run_id": id, "why": "an order went out"}),
        )
        .unwrap();
    assert!(sent["total"].as_u64().unwrap() > 0);
    let asked = lab
        .dispatch("get_decisions", &json!({"run_id": id, "wanted_only": true}))
        .unwrap();
    // Its reasons count only the bars it lists.
    let counted: u64 = asked["reasons"]
        .as_object()
        .unwrap()
        .values()
        .map(|count| count.as_u64().unwrap())
        .sum();
    assert_eq!(counted, asked["total"].as_u64().unwrap(), "{asked}");
    assert!(asked["reasons"].get("warming up").is_none(), "{asked}");
    assert!(
        asked["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| !row[2].is_null())
    );

    let plain = backtest(&lab, &ema());
    let wrong = lab
        .dispatch("get_decisions", &json!({"run_id": plain["run_id"]}))
        .unwrap_err();
    assert!(wrong.contains("decision_log"), "{wrong}");
}

#[test]
fn logic_can_use_what_was_added_from_lab_and_a_longer_timeframe_is_validated() {
    let (lab, _dir) = lab();
    let tree = json!({"logic": {
        "name": "Above the daily average, when it's calm",
        "long": {"and": [
            {"gt": ["close", {"on": {"timeframe": "1d", "value": {"sma": {"length": 2}}}}]},
            {"lt": [{"stddev": {"length": 10}}, {"max": [1000, {"abs": {"sub": ["close", "open"]}}]}]},
        ]},
        "flat": {"lt": ["close", {"on": {"timeframe": "1d", "value": "low"}}]},
    }});
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": tree, "wait_seconds": 120})),
        )
        .unwrap();
    assert!(run["summary"]["fills"].as_u64().unwrap() > 0, "{run}");
}

#[test]
fn a_long_look_back_on_a_longer_timeframe_is_warned_of_in_the_charts_bars() {
    let (lab, _dir) = lab();
    let tree = json!({"logic": {
        "name": "Long on a long daily average",
        "long": {"gt": ["close", {"on": {"timeframe": "1d", "value": {"sma": {"length": 300}}}}]},
    }});
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({"strategy": tree, "wait_seconds": 120})),
        )
        .unwrap();
    let warnings = run["summary"]["warnings"].to_string();
    assert!(warnings.contains("look-back"), "{warnings}");
    assert!(warnings.contains("7200"), "{warnings}");
}

#[test]
fn settings_switched_on_but_ignored_as_stock_is_off_are_named() {
    let (lab, _dir) = lab();
    let run = lab
        .dispatch(
            "run_backtest",
            &with(json!({
                "strategy": ema(),
                "settings": {"stock": false, "leverage": {"on": true, "value": 3}},
                "wait_seconds": 120,
            })),
        )
        .unwrap();
    let warnings = run["summary"]["warnings"].to_string();
    assert!(
        warnings.contains("`leverage`") && warnings.contains("`stock` is off"),
        "{warnings}"
    );
    let lab = two_hand_markets();
    let run = lab
        .dispatch(
            "run_portfolio",
            &pair(json!({"settings": {"stock": false, "leverage": {"on": true, "value": 3}}})),
        )
        .unwrap();
    assert!(
        run["summary"]["warnings"]
            .to_string()
            .contains("`leverage`"),
        "{run}"
    );
}

#[test]
fn the_news_the_app_collected_reads_by_time_words_and_importance() {
    use persephone_data::news::{Headline, NewsStore};
    let (lab, dir) = lab();
    let at = |day: u64| 1_704_067_200_000_000_000 + day * 86_400_000_000_000;
    let headline = |day: u64, title: &str| Headline {
        published: at(day),
        seen: at(day),
        source: "Wire".into(),
        title: title.into(),
        link: format!("https://example.com/{day}"),
        id: format!("n{day}"),
        official: false,
    };
    // As the app keeps them, under the same data folder.
    NewsStore::open(&dir)
        .add(vec![
            headline(1, "Fed holds rates"),
            headline(2, "Bitcoin miners sell"),
            headline(3, "NFT sales fall"),
            headline(4, "Ether staking grows"),
        ])
        .unwrap();
    let get = |args: Value| lab.dispatch("get_news", &args).unwrap();
    let all = get(json!({}));
    assert_eq!(all["total"], 4);
    assert_eq!(all["rows"][0][3], "Ether staking grows", "newest first");
    assert_eq!(all["collected_since"], "2024-01-02 00:00");
    let span = get(json!({"from": "2024-01-03", "to": "2024-01-05"}));
    assert_eq!(span["total"], 2);
    assert_eq!(get(json!({"query": "BITCOIN"}))["total"], 1);
    let for_btc = get(json!({"symbol": "BTCUSDT", "importance": "medium"}));
    let titles: Vec<&str> = for_btc["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[3].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Bitcoin miners sell", "Fed holds rates"]);
    assert_eq!(for_btc["rows"][1][4], "high");
    assert!(
        lab.dispatch("get_news", &json!({"importance": "huge"}))
            .is_err()
    );
}
