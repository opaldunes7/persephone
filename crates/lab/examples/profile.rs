//! Times what long research runs do most, to see what's worth speeding up:
//!
//! ```text
//! cargo run --release -p persephone-lab --example profile [--binance] [--agent]
//! ```
//!
//! `--binance` also times repeated backtests of a real market, which needs the network. `--agent`
//! also times what grows with an agent's use: thousands of runs, a long call log, millions of ticks.

use std::{
    fs::{self, File},
    io::{BufWriter, Write as _},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use flate2::{Compression, write::GzEncoder};
use persephone_core::time::civil_from_days;
use persephone_lab::{Lab, Paths, tools::tools};
use persephone_plugin_host::Host;
use persephone_research::RunColumn;
use persephone_research::robustness::{RunRow, monte_carlo, sort_rows, summarize_run};
use serde_json::{Value, json};

const HOUR: u64 = 3_600_000_000_000;
/// 2000-01-01.
const START: u64 = 946_684_800_000_000_000;

fn timed<T>(name: &str, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let answer = work();
    println!("{name:<52} {:>10.2?}", started.elapsed());
    answer
}

fn average(name: &str, times: u32, mut work: impl FnMut()) {
    let started = Instant::now();
    for _ in 0..times {
        work();
    }
    let each: Duration = started.elapsed() / times;
    println!("{name:<52} {each:>10.2?}  (each of {times})");
}

fn csv(bars: u64) -> String {
    let mut text = String::from("time,open,high,low,close,volume\n");
    for i in 0..bars {
        let mid = 50_000.0 + 1_500.0 * (i as f64 / 30.0).sin() + 200.0 * (i as f64 / 7.0).cos();
        let at = START + i * HOUR;
        let (year, month, day) = civil_from_days((at / (24 * HOUR)) as i64);
        let hour = (at % (24 * HOUR)) / HOUR;
        text += &format!(
            "{year:04}-{month:02}-{day:02} {hour:02}:00:00,{:.2},{:.2},{:.2},{:.2},{:.5}\n",
            mid - 5.0,
            mid + 20.0,
            mid - 20.0,
            mid + 5.0,
            10.12345 + (i % 7) as f64,
        );
    }
    text
}

fn call(lab: &Arc<Lab>, tool: &str, args: &Value) -> Value {
    lab.dispatch(tool, args)
        .unwrap_or_else(|e| panic!("{tool}: {e}"))
}

/// What an agent's work costs once there's a lot of it.
fn agent(lab: &Arc<Lab>, dir: &Path) {
    println!("\nWhat an agent's own use makes slower");
    let args = json!({
        "symbol": "BTCUSDT", "venue": "TEST", "bars": "1h", "from": "2000-01-01",
        "strategy": {"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 20}},
        "wait_seconds": 600,
    });
    let run = call(lab, "run_backtest", &args);
    let id = run["run_id"].as_str().unwrap().to_owned();
    timed("First read after a 100,000-bar backtest", || {
        call(lab, "get_report", &json!({"run_id": id}))
    });

    average("A page of that run's trades", 5, || {
        call(lab, "get_trades", &json!({"run_id": id, "limit": 20}));
    });

    println!("\nWhat one call costs in text, with no limit given");
    for (tool, args) in [
        (
            "get_bars",
            json!({"symbol": "BTCUSDT", "venue": "TEST", "bars": "1h", "from": "2005-01-01"}),
        ),
        ("get_trades", json!({"run_id": id})),
        ("get_fills", json!({"run_id": id})),
        ("get_equity", json!({"run_id": id})),
        ("get_returns", json!({"run_id": id})),
    ] {
        let bytes = call(lab, tool, &args).to_string().len();
        println!(
            "  {tool:<50} {:>7} KB, about {:>6} tokens",
            bytes / 1024,
            bytes / 4
        );
    }
    let grid = call(
        lab,
        "run_optimize",
        &json!({
            "symbol": "BTCUSDT", "venue": "TEST", "bars": "1h", "from": "2010-01-01",
            "strategy": {"builtin": "ema-cross"}, "wait_seconds": 600,
            "variables": [
                {"name": "Fast", "min": 2, "max": 11, "step": 1},
                {"name": "Slow", "min": 20, "max": 29, "step": 1},
            ],
        }),
    );
    let bytes = grid.to_string().len();
    println!(
        "  {:<50} {:>7} KB, about {:>6} tokens",
        "run_optimize, 100 values",
        bytes / 1024,
        bytes / 4
    );

    let list: Vec<Value> = tools()
        .iter()
        .map(|tool| json!({"name": tool.name, "description": tool.what, "inputSchema": tool.input_schema()}))
        .collect();
    let bytes = Value::Array(list).to_string().len();
    println!(
        "  {:<50} {:>7} KB, about {:>6} tokens",
        "The tool list, once a session",
        bytes / 1024,
        bytes / 4
    );

    let short = json!({
        "symbol": "BTCUSDT", "venue": "TEST", "bars": "1h", "from": "2000-03-01", "to": "2000-03-05",
        "strategy": {"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 20}},
    });
    average("A very short backtest, saved", 20, || {
        call(lab, "run_backtest", &short);
    });
    let mut list = json!({
        "symbol": "BTCUSDT", "venue": "TEST", "bars": "1h", "from": "2000-03-01", "to": "2000-06-01",
        "strategy": {"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 20}},
    });
    timed("200 backtests of about 2,700 bars, one call each", || {
        for fast in 0..200 {
            list["strategy"]["params"]["Fast"] = json!(2 + fast % 15);
            call(lab, "run_backtest", &list);
        }
    });
    let batch: Vec<Value> = (0..200)
        .map(|fast| json!({"strategy": {"builtin": "ema-cross", "params": {"Fast": 2 + fast % 15, "Slow": 20}}}))
        .collect();
    let answer = timed("The same 200 in one run_backtests call", || {
        call(
            lab,
            "run_backtests",
            &json!({
                "runs": batch, "wait_seconds": 600, "limit": 10,
                "defaults": {"symbol": "BTCUSDT", "venue": "TEST", "bars": "1h", "from": "2000-03-01", "to": "2000-06-01"},
            }),
        )
    });
    println!(
        "  ({} succeeded, {} bytes)",
        answer["succeeded"],
        answer.to_string().len()
    );
    let optimize = json!({
        "symbol": "BTCUSDT", "venue": "TEST", "bars": "1h", "from": "2000-01-01",
        "strategy": {"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 40}}, "wait_seconds": 600,
        "variables": [{"name": "Fast", "min": 2, "max": 21, "step": 1}],
    });
    timed("Optimize 20 values on local bars, alone", || {
        call(lab, "run_optimize", &optimize)
    });
    timed("Optimize 20 values, three at once", || {
        std::thread::scope(|scope| {
            for _ in 0..3 {
                scope.spawn(|| call(lab, "run_optimize", &optimize));
            }
        });
    });

    // Saving, as run_backtest does: a tree, its text, then packed.
    let times: Vec<u64> = (0..100_000).map(|i| START + i * HOUR).collect();
    let equity: Vec<f64> = (0..100_000)
        .map(|i| 100_000.0 + (f64::from(i) / 40.0).sin() * 1234.567)
        .collect();
    timed("Build, write and pack 100,000 bars of a run", || {
        let run = json!({"times": times, "equity": equity});
        let mut packed = GzEncoder::new(Vec::new(), Compression::fast());
        packed.write_all(run.to_string().as_bytes()).unwrap();
        packed.finish().unwrap()
    });

    // Thousands of runs, made by copying a small one under new ids.
    let runs = lab.paths.lab().join("runs");
    let small = call(lab, "run_backtest", &short);
    let small = small["run_id"].as_str().unwrap().to_owned();
    let packed = fs::read(runs.join(format!("{small}.json.gz"))).unwrap();
    let brief = fs::read(runs.join(format!("{small}.summary"))).unwrap();
    let ids: Vec<String> = (0..10_000)
        .map(|i| format!("run-{}-1-{i}", 1_700_000_000_000_u64 + i))
        .collect();
    for id in &ids {
        fs::write(runs.join(format!("{id}.json.gz")), &packed).unwrap();
        fs::write(runs.join(format!("{id}.summary")), &brief).unwrap();
    }
    average("list_runs with 10,000 runs saved", 3, || {
        call(lab, "list_runs", &json!({"limit": 20}));
    });
    average("compare_runs of 20 runs", 3, || {
        call(lab, "compare_runs", &json!({"run_ids": ids[100..120]}));
    });

    // A long call log.
    let line = json!({"at": 1, "tool": "run_backtest", "args": short, "ok": true, "error": null})
        .to_string();
    {
        let mut log = BufWriter::new(
            File::options()
                .create(true)
                .append(true)
                .open(lab.paths.lab().join("calls.jsonl"))
                .unwrap(),
        );
        for _ in 0..1_000_000 {
            writeln!(log, "{line}").unwrap();
        }
    }
    timed("get_call_log with 1,000,000 calls logged", || {
        call(lab, "get_call_log", &json!({"limit": 50}))
    });

    // Millions of ticks, over about a year.
    const TICKS: u64 = 10_000_000;
    let file = dir.join("ticks.csv");
    {
        let mut out = BufWriter::new(File::create(&file).unwrap());
        writeln!(out, "time,price,size").unwrap();
        for i in 0..TICKS {
            let at = START + i * 3_000_000_000;
            let (year, month, day) = civil_from_days((at / (24 * HOUR)) as i64);
            let seconds = (at % (24 * HOUR)) / 1_000_000_000;
            writeln!(
                out,
                "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02},{:.2},0.5",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60,
                50_000.0 + 1_500.0 * (i as f64 / 3_000.0).sin(),
            )
            .unwrap();
        }
    }
    timed("Import 10,000,000 ticks", || {
        call(
            lab,
            "import_data",
            &json!({
                "files": [file], "symbol": "ETHUSDT", "venue": "TEST", "wait_seconds": 3600,
            }),
        )
    });
    let mut ticks =
        json!({"symbol": "ETHUSDT", "venue": "TEST", "from": "2000-01-01", "limit": 100});
    timed("A page of 100 ticks from the start, no `to`", || {
        call(lab, "get_ticks", &ticks)
    });
    ticks["from"] = json!("2000-12-30");
    timed("A page of 100 ticks near the end, no `to`", || {
        call(lab, "get_ticks", &ticks)
    });
    let every = json!({
        "symbol": "ETHUSDT", "venue": "TEST", "bars": "1000t", "from": "2000-03-01", "to": "2000-04-01",
        "strategy": {"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 20}}, "wait_seconds": 600,
    });
    timed("Backtest a month of tick bars", || {
        call(lab, "run_backtest", &every)
    });
    let _ = lab;
}

fn main() {
    let dir = std::env::temp_dir().join(format!("persephone-profile-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    if let Ok(paths) = Paths::default_paths() {
        let host = Host::new().unwrap();
        let loaded = timed("Compile the plugins folder (every CLI call does)", || {
            host.load_dir(&paths.plugins)
        });
        println!(
            "  ({} indicators, {} strategies)",
            loaded.indicators.len(),
            loaded.strategies.len()
        );
    }
    let lab = timed("Start Lab, plugins included", || {
        Lab::new(Paths::under(&dir)).unwrap()
    });

    const BARS: u64 = 100_000;
    let file = dir.join("bars.csv");
    fs::write(&file, csv(BARS)).unwrap();
    timed("Import 100,000 bars", || {
        call(
            &lab,
            "import_data",
            &json!({
                "files": [file], "symbol": "BTCUSDT", "venue": "TEST", "wait_seconds": 600,
            }),
        )
    });
    let args = json!({
        "symbol": "BTCUSDT", "venue": "TEST", "bars": "1h", "from": "2000-01-01",
        "strategy": {"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 20}},
        "wait_seconds": 600,
    });
    let run = timed("Backtest 100,000 local bars, saved", || {
        call(&lab, "run_backtest", &args)
    });
    let id = run["run_id"].as_str().unwrap().to_owned();
    let size = |id: &str| {
        fs::read_dir(lab.paths.lab().join("runs"))
            .unwrap()
            .flatten()
            .filter(|f| f.file_name().to_string_lossy().starts_with(id))
            .map(|f| f.metadata().unwrap().len())
            .sum::<u64>()
    };
    println!("  (saved run: {} KB)", size(&id) / 1024);
    println!("  (trades: {})", run["summary"]["trades"]);
    average("Read a page of a run's equity", 5, || {
        call(&lab, "get_equity", &json!({"run_id": id, "limit": 100}));
    });
    average("Read a run's report", 5, || {
        call(&lab, "get_report", &json!({"run_id": id}));
    });
    average("Read a page of 1,000 bars", 5, || {
        call(
            &lab,
            "get_bars",
            &json!({
                "symbol": "BTCUSDT", "venue": "TEST", "bars": "1h", "from": "2005-01-01", "limit": 1000,
            }),
        );
    });
    timed("Optimize 20 values on local bars", || {
        let mut more = args.clone();
        more["variables"] = json!([{"name": "Fast", "min": 2, "max": 21, "step": 1}]);
        more["strategy"] = json!({"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 40}});
        call(&lab, "run_optimize", &more)
    });
    timed("Optimize 20 values, scored by Monte Carlo", || {
        let mut more = args.clone();
        more["variables"] = json!([{"name": "Fast", "min": 2, "max": 21, "step": 1}]);
        more["strategy"] = json!({"builtin": "ema-cross", "params": {"Fast": 5, "Slow": 40}});
        more["score"] = json!("monte-carlo");
        call(&lab, "run_optimize", &more)
    });

    // What the panel works out for Monte Carlo's table each time it draws.
    let trades: Vec<f64> = (0..500).map(|i| f64::from(i % 17 - 8) * 25.0).collect();
    let found = timed("Monte Carlo: 10,000 runs of 500 trades", || {
        monte_carlo(&trades, 100_000.0, 10_000, 0.5, 1).unwrap()
    });
    let mut rows: Vec<RunRow> = Vec::new();
    average("  Rank all runs (the panel: twice each update)", 20, || {
        rows = found.ranked_runs();
    });
    average(
        "  Sort them by fall (the panel: once each update)",
        20,
        || {
            sort_rows(&mut rows, (RunColumn::Fell, false));
        },
    );
    average("  Work out a run's draws and figures", 20, || {
        let draws = found.draws_of_run(5).unwrap();
        std::hint::black_box(summarize_run(&draws, &trades));
    });
    average(
        "  Fan: scan all drawn points for a price range",
        200,
        || {
            let mut lo = f64::MAX;
            for path in &found.paths {
                for &value in path {
                    lo = lo.min(value);
                }
            }
            std::hint::black_box(lo);
        },
    );

    if std::env::args().any(|arg| arg == "--agent") {
        agent(&lab, &dir);
    }
    if std::env::args().any(|arg| arg == "--binance") {
        let args = json!({
            "symbol": "BTCUSDT", "bars": "1h", "from": "2023-01-01",
            "strategy": {"builtin": "ema-cross"}, "wait_seconds": 600,
        });
        timed("Backtest a Binance market, 3+ years of 1h: cold", || {
            call(&lab, "run_backtest", &args)
        });
        average("  Again, with its bars cached", 5, || {
            call(&lab, "run_backtest", &args);
        });
    }
    let _ = fs::remove_dir_all(&dir);
}
