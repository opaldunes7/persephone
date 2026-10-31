//! Tools that run strategies, and read what they did. Runs are saved by id, so they can be referred
//! to later and survive restarts.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read as _, Write as _},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use flate2::{Compression, read::GzDecoder, write::GzEncoder};

use persephone_core::{
    Candle, Side, Timeframe,
    signal::{Note, SignalFactory},
    strategy::{StrategyKind, StrategySpec},
};
use persephone_engine::{BacktestReport, BacktestRequest, Fill};
use persephone_research::{
    WARM_UP_BARS,
    calendar::{Calendar, days_in_month, weeks},
    market::{MAX_FINE_BARS, MarketBars, fine_bars},
    optimize::{Grid, OptimizeJob, Outcome, split_at},
    results::{StrategyRef, with_params},
    robustness::{
        BlockJob, MonteCarlo, RoundTrip, backtest, closed_trades, monte_carlo, pick_blocks,
        round_trips,
    },
    settings::{BlockLength, OptimizeSettings, Precision, ScoreBy, Settings, Variable},
};
use serde_json::{Map, Value, json};

use crate::{
    Lab,
    data::MarketArgs,
    jobs::{Job, check_id},
    new_id, now_ms, time_text,
    tools::{page, page_of, text},
};

/// How many runs to keep read.
const RECENT_RUNS: usize = 4;
const TRADE_COLUMNS: [&str; 14] = [
    "trade",
    "side",
    "opened",
    "opened_ts",
    "closed",
    "closed_ts",
    "entry_bar",
    "exit_bar",
    "size",
    "entry_price",
    "exit_price",
    "profit",
    "return_percent",
    "fees",
];

/// What a backtest needs from what a call asked for.
///
/// # Errors
///
/// Returns a message if the settings don't make sense.
pub fn build_request(
    market: MarketBars,
    strategy: StrategySpec,
    plugin: Option<Arc<dyn SignalFactory>>,
    settings: &Settings,
    trade_from: u64,
    fill_bars: Option<Arc<Vec<Candle>>>,
) -> Result<BacktestRequest, String> {
    let high = settings.precision == Precision::High;
    Ok(BacktestRequest {
        fees: settings.fees(market.fees),
        ticks: market.ticks.filter(|_| high),
        instrument: market.instrument,
        bars: market.bars,
        candles: market.candles,
        strategy,
        plugin,
        starting_balance: settings.capital,
        sim: settings.sim(),
        rules: settings.rules()?,
        trade_from,
        fill_bars,
    })
}

fn settings_of(args: &Value) -> Result<Settings, String> {
    args.get("settings").map_or_else(
        || Ok(Settings::default()),
        |value| serde_json::from_value(value.clone()).map_err(|e| format!("`settings`: {e}")),
    )
}

fn parse_length(args: &Value) -> Result<BlockLength, String> {
    match args.get("length").and_then(Value::as_str) {
        None | Some("month") => Ok(BlockLength::Month),
        Some("week") => Ok(BlockLength::Week),
        Some("quarter") => Ok(BlockLength::Quarter),
        Some(other) => Err(format!("`length`: `{other}` isn't week, month or quarter")),
    }
}

fn number(args: &Value, name: &str, default: u64) -> u64 {
    args.get(name).and_then(Value::as_u64).unwrap_or(default)
}

fn side_text(long: bool) -> &'static str {
    if long { "long" } else { "short" }
}

fn trade_row(number: usize, trade: &RoundTrip, capital: f64) -> Value {
    json!([
        number,
        side_text(trade.long),
        time_text(trade.entry_ts),
        trade.entry_ts,
        if trade.open {
            "open".to_owned()
        } else {
            time_text(trade.exit_ts)
        },
        (!trade.open).then_some(trade.exit_ts),
        trade.entry_bar,
        trade.exit_bar,
        trade.qty,
        trade.entry_price,
        (!trade.open).then_some(trade.exit_price),
        trade.profit,
        trade.profit / capital * 100.0,
        trade.fees,
    ])
}

/// What a report comes to, in a few figures.
fn summary_of(report: &BacktestReport, from_index: usize, name: &str) -> Value {
    let trades = round_trips(report);
    let winners = trades.iter().filter(|t| t.profit > 0.0).count();
    let last = report
        .equity
        .last()
        .copied()
        .unwrap_or(report.starting_balance);
    json!({
        "strategy": name,
        "net_return": last / report.starting_balance - 1.0,
        "final_equity": last,
        // A fall is a positive fraction everywhere in Lab, though the engine's is negative.
        "max_drawdown": report.max_drawdown.abs(),
        "fees": report.fees,
        "fills": report.fills.len(),
        "trades": trades.len(),
        "winners": winners,
        "win_rate": winners as f64 / trades.len().max(1) as f64,
        "bars": report.equity.len().saturating_sub(from_index),
    })
}

fn map_of(map: &BTreeMap<String, f64>) -> Value {
    Value::Object(map.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

fn map_from(value: &Value) -> BTreeMap<String, f64> {
    value.as_object().map_or_else(BTreeMap::new, |map| {
        map.iter()
            .map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(f64::NAN)))
            .collect()
    })
}

/// A report read back from its file: all that the tools built on a run need.
fn report_from(run: &Value) -> BacktestReport {
    let numbers = |name: &str| -> Vec<f64> {
        run[name].as_array().map_or_else(Vec::new, |values| {
            values
                .iter()
                .map(|v| v.as_f64().unwrap_or(f64::NAN))
                .collect()
        })
    };
    let fills = run["fills"].as_array().map_or_else(Vec::new, |fills| {
        fills
            .iter()
            .map(|fill| Fill {
                ts: fill[0].as_u64().unwrap_or(0),
                bar: fill[1].as_u64().unwrap_or(0) as usize,
                side: if fill[2] == "buy" {
                    Side::Buy
                } else {
                    Side::Sell
                },
                price: fill[3].as_f64().unwrap_or(f64::NAN),
                qty: fill[4].as_f64().unwrap_or(f64::NAN),
                fee: fill[5].as_f64().unwrap_or(0.0),
            })
            .collect()
    });
    let series = run["returns_series"]
        .as_array()
        .map_or_else(Vec::new, |rows| {
            rows.iter()
                .map(|row| {
                    (
                        row[0].as_u64().unwrap_or(0),
                        row[1].as_f64().unwrap_or(f64::NAN),
                    )
                })
                .collect()
        });
    let notes = run["notes"].as_array().map_or_else(Vec::new, |notes| {
        notes
            .iter()
            .map(|note| Note {
                text: note["text"].as_str().unwrap_or_default().to_owned(),
                warning: note["warning"].as_bool().unwrap_or(false),
            })
            .collect()
    });
    BacktestReport {
        fills,
        equity: numbers("equity"),
        starting_balance: run["starting_balance"].as_f64().unwrap_or(0.0),
        max_drawdown: run["max_drawdown"].as_f64().unwrap_or(0.0),
        fees: run["fees"].as_f64().unwrap_or(0.0),
        total_orders: run["total_orders"].as_u64().unwrap_or(0) as usize,
        total_positions: run["total_positions"].as_u64().unwrap_or(0) as usize,
        pnl: map_from(&run["pnl"]),
        returns: map_from(&run["returns"]),
        general: map_from(&run["general"]),
        returns_series: series,
        notes,
    }
}

impl Lab {
    /// The strategy a call names, and what makes its signal if it isn't built in.
    pub(crate) fn resolve(
        &self,
        value: &Value,
    ) -> Result<(StrategySpec, Option<Arc<dyn SignalFactory>>), String> {
        match StrategyRef::parse(value)? {
            StrategyRef::BuiltIn(kind) => Ok((
                with_params(StrategyKind::BuiltIn(kind), Arc::new(kind.info()), value)?,
                None,
            )),
            StrategyRef::Plugin(id) => {
                let plugins = self.plugins();
                let plugin = plugins.strategies.get(&id).ok_or_else(|| {
                    let known: Vec<&String> = plugins.strategies.keys().collect();
                    format!("strategy: there's no plugin `{id}`; there are {known:?}")
                })?;
                let spec = with_params(
                    StrategyKind::Plugin(id.clone()),
                    Arc::clone(&plugin.info),
                    value,
                )?;
                Ok((spec, Some(Arc::clone(plugin) as Arc<dyn SignalFactory>)))
            }
            StrategyRef::Logic(logic) => {
                let spec = with_params(logic.spec().kind, Arc::new(logic.info()), value)?;
                let plugin: Arc<dyn SignalFactory> = Arc::new(*logic);
                Ok((spec, Some(plugin)))
            }
        }
    }

    /// How many backtests may run at once: one for a strategy in a process of its own, which may
    /// not take more.
    fn workers_for(&self, spec: &StrategySpec) -> usize {
        if let StrategyKind::Plugin(id) = &spec.kind
            && self
                .plugins()
                .strategies
                .get(id)
                .is_some_and(|plugin| plugin.address().is_some())
        {
            return 1;
        }
        std::thread::available_parallelism()
            .map_or(1, usize::from)
            .saturating_sub(1)
            .max(1)
    }

    /// Everything a backtest of what a call asks for needs.
    fn prepare(&self, args: &Value) -> Result<(BacktestRequest, usize, String), String> {
        let market_args = MarketArgs::parse(args)?;
        let settings = settings_of(args)?;
        let (spec, plugin) = self.resolve(args.get("strategy").unwrap_or(&Value::Null))?;
        let name = spec.info.name.clone();
        let market = self.load_market(&market_args)?;
        let from = market_args.from.unwrap_or(0);
        let from_index = market.candles.partition_point(|c| c.ts < from);
        // High precision fills on ticks, or on 1-minute bars where there are none.
        let fine = if settings.precision == Precision::High
            && market.ticks.is_none()
            && market.bars != Timeframe::M1.into()
        {
            let first = market.candles.first().map_or(0, |c| c.ts);
            let last = market
                .bars
                .closes(&market.candles)
                .last()
                .copied()
                .unwrap_or(first);
            Some(Arc::new(match market_args.venue.as_deref() {
                Some(venue) => {
                    fine_bars(Some((&self.store, venue)), &market_args.symbol, first, last)
                        .map_err(|e| format!("{e:#}"))?
                }
                None => {
                    let minutes = last.saturating_sub(first) / Timeframe::M1.nanos();
                    if minutes > MAX_FINE_BARS {
                        return Err(format!(
                            "High precision would need {minutes} 1-minute bars here, more than {MAX_FINE_BARS}; pick a shorter span"
                        ));
                    }
                    self.cache
                        .bars(&market_args.symbol, Timeframe::M1, first, last)?
                }
            }))
        } else {
            None
        };
        let request = build_request(market, spec, plugin, &settings, from, fine)?;
        Ok((request, from_index, name))
    }

    pub(crate) fn run_backtest(&self, args: &Value) -> Result<Value, String> {
        let (request, from_index, name) = self.prepare(args)?;
        let times: Vec<u64> = request.candles.iter().map(|c| c.ts).collect();
        let report = backtest(request)?;
        let mut summary = summary_of(&report, from_index, &name);
        // Which bars these were, as a span with no end moves on as new bars close.
        summary["first_bar"] = json!(times.get(from_index).copied().map(time_text));
        summary["last_bar"] = json!(times.last().copied().map(time_text));
        let run_id = new_id("run");
        let run = json!({
            "run_id": run_id,
            "created": now_ms(),
            "request": args,
            "summary": summary,
            "from_index": from_index,
            "times": times,
            "equity": report.equity,
            "fills": report.fills.iter().map(|f| json!([
                f.ts, f.bar, if f.side == Side::Buy { "buy" } else { "sell" }, f.price, f.qty, f.fee,
            ])).collect::<Vec<_>>(),
            "starting_balance": report.starting_balance,
            "max_drawdown": report.max_drawdown,
            "fees": report.fees,
            "total_orders": report.total_orders,
            "total_positions": report.total_positions,
            "pnl": map_of(&report.pnl),
            "returns": map_of(&report.returns),
            "general": map_of(&report.general),
            "returns_series": report.returns_series,
            "notes": report.notes.iter()
                .map(|n| json!({"text": n.text, "warning": n.warning}))
                .collect::<Vec<_>>(),
        });
        let dir = self.paths.lab().join("runs");
        let mut packed = GzEncoder::new(Vec::new(), Compression::fast());
        packed
            .write_all(run.to_string().as_bytes())
            .and_then(|()| packed.finish())
            .and_then(|bytes| fs::write(dir.join(format!("{run_id}.json.gz")), bytes))
            .and_then(|()| {
                let brief = json!({"run_id": run_id, "created": now_ms(), "summary": summary});
                fs::write(dir.join(format!("{run_id}.summary")), brief.to_string())
            })
            .map_err(|e| format!("couldn't save the run: {e}"))?;
        Ok(json!({"run_id": run_id, "summary": summary}))
    }

    pub(crate) fn load_run(&self, id: &str) -> Result<Arc<Value>, String> {
        check_id(id)?;
        if let Ok(recent) = self.recent_runs.lock()
            && let Some((_, run)) = recent.iter().find(|(known, _)| known == id)
        {
            return Ok(Arc::clone(run));
        }
        let dir = self.paths.lab().join("runs");
        let missing = || format!("there's no run `{id}`");
        let text = match File::open(dir.join(format!("{id}.json.gz"))) {
            Ok(file) => {
                let mut text = String::new();
                GzDecoder::new(file)
                    .read_to_string(&mut text)
                    .map_err(|e| format!("couldn't read the run: {e}"))?;
                text
            }
            // As they were saved before they were packed.
            Err(_) => fs::read_to_string(dir.join(format!("{id}.json"))).map_err(|_| missing())?,
        };
        let run: Arc<Value> = Arc::new(serde_json::from_str(&text).map_err(|e| e.to_string())?);
        if let Ok(mut recent) = self.recent_runs.lock() {
            recent.push_front((id.to_owned(), Arc::clone(&run)));
            recent.truncate(RECENT_RUNS);
        }
        Ok(run)
    }

    /// Removes Lab's own runs and job files older than `age`, for keeping its folder from growing
    /// without limit. Markets, saved results and the call log aren't touched. Says how many it removed
    /// and how many bytes that freed.
    ///
    /// # Errors
    ///
    /// Returns a message if a folder can't be read.
    pub fn prune(&self, age: Duration) -> Result<(usize, u64), String> {
        let (mut files, mut bytes) = (0, 0);
        for folder in ["runs", "jobs"] {
            for entry in fs::read_dir(self.paths.lab().join(folder))
                .map_err(|e| e.to_string())?
                .flatten()
            {
                let Ok(meta) = entry.metadata() else { continue };
                let old = meta
                    .modified()
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .is_some_and(|elapsed| elapsed >= age);
                if meta.is_file() && old && fs::remove_file(entry.path()).is_ok() {
                    files += 1;
                    bytes += meta.len();
                }
            }
        }
        if let Ok(mut recent) = self.recent_runs.lock() {
            recent.clear();
        }
        Ok((files, bytes))
    }

    pub(crate) fn get_report(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        Ok(json!({
            "run_id": run["run_id"],
            "request": run["request"],
            "summary": run["summary"],
            "starting_balance": run["starting_balance"],
            "max_drawdown": run["max_drawdown"].as_f64().map(f64::abs),
            "fees": run["fees"],
            "total_orders": run["total_orders"],
            "total_positions": run["total_positions"],
            "pnl": run["pnl"],
            "returns": run["returns"],
            "general": run["general"],
            "notes": run["notes"],
        }))
    }

    pub(crate) fn get_trades(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let report = report_from(&run);
        let rows = round_trips(&report)
            .iter()
            .enumerate()
            .map(|(i, trade)| trade_row(i + 1, trade, report.starting_balance))
            .collect();
        Ok(page(&TRADE_COLUMNS, rows, args, 1_000))
    }

    pub(crate) fn get_fills(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let rows = run["fills"].as_array().cloned().unwrap_or_default();
        Ok(page(
            &["ts", "bar", "side", "price", "size", "fee"],
            rows,
            args,
            1_000,
        ))
    }

    pub(crate) fn get_equity(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let from = run["from_index"].as_u64().unwrap_or(0) as usize;
        let empty = Vec::new();
        let times = run["times"].as_array().unwrap_or(&empty);
        let equity = run["equity"].as_array().unwrap_or(&empty);
        let total = times.len().min(equity.len()).saturating_sub(from);
        Ok(page_of(
            &["ts", "time", "equity"],
            total,
            args,
            1_000,
            |i| {
                let at = times[from + i].as_u64().unwrap_or(0);
                json!([at, time_text(at), equity[from + i]])
            },
        ))
    }

    pub(crate) fn get_returns(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let empty = Vec::new();
        let rows = run["returns_series"].as_array().unwrap_or(&empty);
        Ok(page_of(&["ts", "return"], rows.len(), args, 1_000, |i| {
            rows[i].clone()
        }))
    }

    pub(crate) fn get_calendar(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let report = report_from(&run);
        let last = run["times"]
            .as_array()
            .and_then(|times| times.last())
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let capital = report.starting_balance;
        let calendar = Calendar::new(round_trips(&report), last);
        let total = |t: persephone_research::calendar::Total| {
            json!({
                "profit": t.profit,
                "return_percent": t.profit / capital * 100.0,
                "trades": t.trades,
            })
        };
        let year = args.get("year").and_then(Value::as_i64);
        let month = args.get("month").and_then(Value::as_u64).map(|m| m as u32);
        let day = args.get("day").and_then(Value::as_u64).map(|d| d as u32);
        match (year, month, day) {
            (Some(year), Some(month), Some(day)) => {
                let rows: Vec<Value> = calendar
                    .trades_on(year, month, day)
                    .iter()
                    .map(|&i| trade_row(i + 1, &calendar.trades[i], capital))
                    .collect();
                Ok(json!({
                    "level": "day",
                    "total": total(calendar.day(year, month, day)),
                    "columns": TRADE_COLUMNS,
                    "rows": rows,
                    "day_before": level_text(calendar.day_before(year, month, day)),
                    "day_after": level_text(calendar.day_after(year, month, day)),
                }))
            }
            (Some(year), Some(month), None) => {
                if !(1..=12).contains(&month) {
                    return Err(format!("`month`: `{month}` isn't from 1 to 12"));
                }
                let rows: Vec<Value> = (1..=days_in_month(year, month))
                    .map(|day| {
                        let mut row = total(calendar.day(year, month, day));
                        row["day"] = json!(day);
                        row
                    })
                    .collect();
                Ok(json!({
                    "level": "month",
                    "total": total(calendar.month(year, month)),
                    "weeks": weeks(year, month),
                    "rows": rows,
                }))
            }
            (None, None, None) => {
                let rows: Vec<Value> = calendar
                    .years()
                    .into_iter()
                    .flat_map(|year| (1..=12).map(move |month| (year, month)))
                    .map(|(year, month)| {
                        let mut row = total(calendar.month(year, month));
                        row["year"] = json!(year);
                        row["month"] = json!(month);
                        row
                    })
                    .collect();
                Ok(json!({"level": "months", "rows": rows}))
            }
            _ => Err("`year` and `month` go together, and `day` needs both".into()),
        }
    }

    pub(crate) fn list_runs(&self, args: &Value) -> Result<Value, String> {
        let dir = self.paths.lab().join("runs");
        let mut runs: Vec<Value> = fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "summary"))
            .filter_map(|entry| serde_json::from_str(&fs::read_to_string(entry.path()).ok()?).ok())
            .collect();
        runs.sort_by_key(|run: &Value| std::cmp::Reverse(run["created"].as_u64().unwrap_or(0)));
        Ok(page(&["run_id", "created", "summary"], runs, args, 100))
    }

    pub(crate) fn compare_runs(&self, args: &Value) -> Result<Value, String> {
        let ids = args
            .get("run_ids")
            .and_then(Value::as_array)
            .ok_or("`run_ids` is missing")?;
        let runs = ids
            .iter()
            .map(|id| {
                let id = id
                    .as_str()
                    .ok_or_else(|| format!("`run_ids`: {id} isn't an id"))?;
                let run = self.load_run(id)?;
                Ok(json!({"run_id": id, "summary": run["summary"], "request": run["request"]}))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(json!({"runs": runs}))
    }

    pub(crate) fn run_monte_carlo(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let report = report_from(&run);
        let draws = number(args, "runs", 1_000) as u32;
        let ruin = args
            .get("ruin_percent")
            .and_then(Value::as_f64)
            .unwrap_or(50.0);
        job.total.store(draws as usize, Ordering::Relaxed);
        let found = monte_carlo(
            &closed_trades(&report),
            report.starting_balance,
            draws,
            ruin / 100.0,
            number(args, "seed", 0),
        )?;
        job.done.store(draws as usize, Ordering::Relaxed);
        monte_carlo_json(&found, &report, args)
    }

    pub(crate) fn run_random_blocks(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let (mut request, _, _) = self.prepare(args)?;
        let workers = self.workers_for(&request.strategy);
        let candles = std::mem::take(&mut request.candles);
        let from = request.trade_from;
        let from_index = candles.partition_point(|c| c.ts < from);
        let blocks = pick_blocks(
            &candles,
            from_index,
            parse_length(args)?,
            number(args, "blocks", 50) as u32,
            number(args, "seed", 0),
        )?;
        job.total.store(blocks.len(), Ordering::Relaxed);
        // On the bars whatever the precision, as the app does, to keep a batch quick.
        request.ticks = None;
        request.fill_bars = None;
        let batch = BlockJob {
            request,
            candles,
            blocks,
            warm_up: WARM_UP_BARS as usize,
            workers,
            stop: Arc::clone(&job.stop),
        };
        let summary = batch.run(&|done| job.done.store(done, Ordering::Relaxed))?;
        let include = args
            .get("include_trades")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let capital = args
            .get("settings")
            .and_then(|settings| settings.get("capital"))
            .and_then(Value::as_f64)
            .unwrap_or_else(|| Settings::default().capital);
        let rows: Vec<Value> = summary
            .blocks
            .iter()
            .zip(&summary.trades)
            .map(|(block, trades)| {
                let mut row = json!({
                    "from": time_text(block.from),
                    "to": time_text(block.to),
                    "return": block.stats.return_,
                    "fell": block.stats.drawdown,
                    "fills": block.fills,
                    "trades": trades.len(),
                });
                if include {
                    row["trade_columns"] = json!(TRADE_COLUMNS);
                    row["trade_rows"] = trades
                        .iter()
                        .enumerate()
                        .map(|(i, trade)| trade_row(i + 1, trade, capital))
                        .collect();
                }
                row
            })
            .collect();
        Ok(json!({
            "profitable": summary.profitable,
            "median": summary.median,
            "best": summary.best,
            "worst": summary.worst,
            "worst_dates": [time_text(summary.worst_dates.0), time_text(summary.worst_dates.1)],
            "blocks": rows,
        }))
    }

    pub(crate) fn run_optimize(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let (mut request, _, _) = self.prepare(args)?;
        let workers = self.workers_for(&request.strategy);
        let variables = args
            .get("variables")
            .and_then(Value::as_array)
            .filter(|v| (1..=2).contains(&v.len()))
            .ok_or("`variables` should be a list of one or two")?;
        let parse_variable = |value: &Value| -> Result<Variable, String> {
            let figure = |name: &str| {
                value[name]
                    .as_f64()
                    .ok_or_else(|| format!("`variables`: `{name}` is missing from {value}"))
            };
            Ok(Variable {
                name: value["name"]
                    .as_str()
                    .ok_or_else(|| format!("`variables`: `name` is missing from {value}"))?
                    .to_owned(),
                min: figure("min")?,
                max: figure("max")?,
                step: figure("step")?,
            })
        };
        let score = match args.get("score").and_then(Value::as_str) {
            None | Some("backtest") => ScoreBy::Backtest,
            Some("monte-carlo") => ScoreBy::MonteCarlo,
            Some("random-blocks") => ScoreBy::RandomBlocks,
            Some(other) => {
                return Err(format!(
                    "`score`: `{other}` isn't backtest, monte-carlo or random-blocks"
                ));
            }
        };
        let hold_out = args
            .get("hold_out")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let chosen = OptimizeSettings {
            variables: [
                Some(parse_variable(&variables[0])?),
                variables.get(1).map(parse_variable).transpose()?,
            ],
            score,
            hold_out,
        };
        let grid = Grid::build(&chosen, &request.strategy.info.params)?;
        let candles = std::mem::take(&mut request.candles);
        let from = candles.partition_point(|c| c.ts < request.trade_from);
        let split = hold_out.then(|| split_at(from, candles.len()));
        let seed = number(args, "seed", 0);
        let blocks = if score == ScoreBy::RandomBlocks {
            pick_blocks(
                &candles[..split.unwrap_or(candles.len())],
                from,
                parse_length(args)?,
                number(args, "blocks", 50) as u32,
                seed,
            )?
        } else {
            Vec::new()
        };
        request.ticks = None;
        request.fill_bars = None;
        let batch = OptimizeJob {
            request,
            candles,
            grid,
            score,
            split,
            blocks,
            warm_up: WARM_UP_BARS as usize,
            runs: number(args, "runs", 1_000) as u32,
            ruin_at: args
                .get("ruin_percent")
                .and_then(Value::as_f64)
                .unwrap_or(50.0)
                / 100.0,
            seed,
            workers,
            stop: Arc::clone(&job.stop),
        };
        let total = batch.total();
        job.total.store(total, Ordering::Relaxed);
        let found = batch.run(&|done| job.done.store(done, Ordering::Relaxed))?;
        let names_of = |cell: usize| -> Value {
            let mut names = Map::new();
            let mut values = found.grid.values_of(cell).into_iter();
            if let Some((_, value)) = values.next() {
                names.insert(found.grid.first.name.clone(), json!(value));
            }
            if let (Some(second), Some((_, value))) = (&found.grid.second, values.next()) {
                names.insert(second.name.clone(), json!(value));
            }
            Value::Object(names)
        };
        let rows: Vec<Value> = found
            .outcomes
            .iter()
            .enumerate()
            .map(|(cell, outcome)| {
                let (score, what, status) = match outcome {
                    Outcome::Scored { score, what } => (Some(*score), what.clone(), "scored"),
                    Outcome::TooFewTrades => {
                        (None, "Too few trades to score.".into(), "too-few-trades")
                    }
                    Outcome::Failed(error) => (None, error.clone(), "failed"),
                };
                json!({"values": names_of(cell), "score": score, "status": status, "what": what})
            })
            .collect();
        let top: Vec<Value> = found
            .top
            .iter()
            .map(|best| {
                json!({
                    "values": names_of(best.cell),
                    "score": best.score,
                    "with_neighbours": best.plateau,
                    "unseen_return": best.unseen,
                })
            })
            .collect();
        Ok(json!({
            "values": rows,
            "top": top,
            "on_edge": found.on_edge,
            "held_out": found.held_out,
            "backtests": total,
        }))
    }
}

fn level_text(level: Option<persephone_research::calendar::Level>) -> Value {
    match level {
        Some(persephone_research::calendar::Level::Day(year, month, day)) => {
            json!({"year": year, "month": month, "day": day})
        }
        _ => Value::Null,
    }
}

fn monte_carlo_json(
    found: &MonteCarlo,
    report: &BacktestReport,
    args: &Value,
) -> Result<Value, String> {
    let ranked = found.ranked_runs();
    let row = |row: &persephone_research::robustness::RunRow| {
        json!({
            "rank": row.rank,
            "run": row.run,
            "backtest": row.backtest,
            "return": row.stats.return_,
            "fell": row.stats.drawdown,
        })
    };
    let limit = number(args, "limit", 20) as usize;
    let actual = ranked.iter().find(|r| r.backtest).map(row);
    let mut answer = json!({
        "trades": found.trades,
        "runs": found.runs,
        "median": found.median,
        "low": found.low,
        "high": found.high,
        "drawdown": found.drawdown,
        "ruin": found.ruin,
        "actual": actual,
        "best_runs": ranked.iter().take(limit).map(row).collect::<Vec<_>>(),
    });
    if let Some(run) = args.get("show_run").and_then(Value::as_u64) {
        let draws = found
            .draws_of_run(run as usize)
            .ok_or_else(|| format!("`show_run`: there's no run {run}"))?;
        let trades = round_trips(report);
        let rows: Vec<Value> = draws
            .iter()
            .enumerate()
            .filter_map(|(place, &index)| {
                Some(trade_row(
                    place + 1,
                    trades.get(index)?,
                    report.starting_balance,
                ))
            })
            .collect();
        answer["run_columns"] = json!(TRADE_COLUMNS);
        answer["run_rows"] = json!(rows);
    }
    Ok(answer)
}
