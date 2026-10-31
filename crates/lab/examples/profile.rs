//! Times what long research runs do most, to see what's worth speeding up:
//!
//! ```text
//! cargo run --release -p persephone-lab --example profile [--binance]
//! ```
//!
//! `--binance` also times repeated backtests of a real market, which needs the network.

use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};

use persephone_core::time::civil_from_days;
use persephone_lab::{Lab, Paths};
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
