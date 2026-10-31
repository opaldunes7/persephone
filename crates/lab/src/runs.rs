//! Tools that run strategies, and read what they did. Runs are saved by id, so they can be referred
//! to later and survive restarts.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read as _, Write as _},
    path::Path,
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
    optimize::{Grid, OptimizeJob, Outcome, rank, split_at},
    positions::Positions,
    results::{StrategyRef, with_params},
    robustness::{
        BlockJob, MonteCarlo, RoundTrip, backtest, closed_trades, monte_carlo, pick_blocks, pool,
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
    tools::{DEFAULT_ROWS, check_args, limit_of, page_of, text, tools},
};

/// How many runs to keep read.
const RECENT_RUNS: usize = 4;
/// How many runs `list_runs` gives unless asked for more.
const LISTED_RUNS: usize = 20;
/// How many values `run_optimize` lists unless asked for more.
const LISTED_VALUES: usize = 50;
/// The most backtests `run_backtests` takes in one call.
const MAX_BACKTESTS: usize = 5_000;
const BATCH_COLUMNS: [&str; 15] = [
    "index",
    "run_id",
    "net_return",
    "max_drawdown",
    "trades",
    "win_rate",
    "fees",
    "final_equity",
    "buy_and_hold_return",
    "sharpe",
    "profit_factor",
    "exposure",
    "average_trade",
    "warnings",
    "error",
];
/// How many windows `run_walk_forward` makes unless asked, and how many times longer each one's
/// picking stretch is than its unseen one.
const WALK_WINDOWS: u64 = 5;
const WALK_RATIO: f64 = 3.0;
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

/// Settings from JSON, refusing what would otherwise quietly do nothing: a misspelt cost flatters
/// a result, and looks just like a setting that worked.
///
/// # Errors
///
/// Returns a message naming the setting.
pub(crate) fn read_settings(value: &Value) -> Result<Settings, String> {
    let defaults = serde_json::to_value(Settings::default()).map_err(|e| e.to_string())?;
    if let (Some(given), Some(known)) = (value.as_object(), defaults.as_object()) {
        for (name, setting) in given {
            let Some(known_one) = known.get(name) else {
                let names: Vec<&String> = known.keys().collect();
                return Err(format!(
                    "`settings`: there's no `{name}`; there are {names:?}"
                ));
            };
            let is_switched = known_one.get("on").is_some() && known_one.get("value").is_some();
            if is_switched && setting.get("on").is_none() {
                return Err(format!(
                    "`settings.{name}`: it needs {{\"on\": true, \"value\": ...}}, or it does nothing"
                ));
            }
        }
    }
    serde_json::from_value(value.clone()).map_err(|e| format!("`settings`: {e}"))
}

fn settings_of(args: &Value) -> Result<Settings, String> {
    args.get("settings")
        .map_or_else(|| Ok(Settings::default()), read_settings)
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
fn summary_of(
    report: &BacktestReport,
    from_index: usize,
    name: &str,
    bars: &[(u64, f64)],
) -> Value {
    let trades = round_trips(report);
    let profit: f64 = trades.iter().map(|t| t.profit).sum();
    let gains: f64 = trades.iter().map(|t| t.profit.max(0.0)).sum();
    let losses: f64 = trades.iter().map(|t| (-t.profit).max(0.0)).sum();
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
        "buy_and_hold_return": buy_and_hold(bars, from_index),
        "sharpe": sharpe(report, from_index, bars),
        "profit_factor": (losses > 0.0).then(|| gains / losses),
        "exposure": exposure(report, from_index),
        "average_trade": (!trades.is_empty()).then(|| profit / trades.len() as f64),
        "warnings": report.notes.iter()
            .filter(|note| note.warning)
            .map(|note| note.text.as_str())
            .collect::<Vec<_>>(),
    })
}

/// What holding the market made: from the close of the first bar it could trade at, as a strategy's
/// first order fills there, to the last close.
fn buy_and_hold(bars: &[(u64, f64)], from_index: usize) -> Option<f64> {
    let first = bars.get(from_index)?.1;
    let last = bars.last()?.1;
    (first > 0.0).then(|| last / first - 1.0)
}

/// The Sharpe ratio of daily returns, with no risk-free rate, over 365 days a year as crypto trades
/// every day. A day's value is its last bar's equity. `None` if there are too few days to say, or
/// nothing ever moved.
fn sharpe(report: &BacktestReport, from_index: usize, bars: &[(u64, f64)]) -> Option<f64> {
    const NANOS_PER_DAY: u64 = 86_400_000_000_000;
    let mut days = vec![report.starting_balance];
    let mut last_day = None;
    for (&(ts, _), &equity) in bars.iter().zip(&report.equity).skip(from_index) {
        let day = ts / NANOS_PER_DAY;
        if last_day == Some(day) {
            *days.last_mut()? = equity;
        } else {
            days.push(equity);
            last_day = Some(day);
        }
    }
    let returns: Vec<f64> = days
        .windows(2)
        .filter(|pair| pair[0] > 0.0)
        .map(|pair| pair[1] / pair[0] - 1.0)
        .collect();
    if returns.len() < 2 {
        return None;
    }
    let count = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / count;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (count - 1.0);
    (variance > 0.0).then(|| mean / variance.sqrt() * 365.0_f64.sqrt())
}

/// The share of the bars traded on that began with a position open. A position's first such bar is
/// the one after the close it filled at.
fn exposure(report: &BacktestReport, from_index: usize) -> f64 {
    let bars = report.equity.len().saturating_sub(from_index);
    if bars == 0 {
        return 0.0;
    }
    let mut position = 0.0;
    let mut fills = report.fills.iter().peekable();
    let mut held = 0;
    for bar in from_index..report.equity.len() {
        while let Some(fill) = fills.next_if(|fill| fill.bar < bar) {
            position += if fill.side == Side::Buy {
                fill.qty
            } else {
                -fill.qty
            };
        }
        // Rounding can leave dust where a position closed.
        if f64::abs(position) > 1e-12 {
            held += 1;
        }
    }
    f64::from(held) / bars as f64
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

/// What a call asks to optimize.
fn optimize_choice(args: &Value, hold_out: bool) -> Result<OptimizeSettings, String> {
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
    Ok(OptimizeSettings {
        variables: [
            Some(parse_variable(&variables[0])?),
            variables.get(1).map(parse_variable).transpose()?,
        ],
        score,
        hold_out,
    })
}

/// A combination's values by name.
fn names_of(grid: &Grid, cell: usize) -> Value {
    let mut names = Map::new();
    let mut values = grid.values_of(cell).into_iter();
    if let Some((_, value)) = values.next() {
        names.insert(grid.first.name.clone(), json!(value));
    }
    if let (Some(second), Some((_, value))) = (&grid.second, values.next()) {
        names.insert(second.name.clone(), json!(value));
    }
    Value::Object(names)
}

/// A summary with its request left out, to keep lists short.
fn without_request(mut brief: Value) -> Value {
    if let Some(brief) = brief.as_object_mut() {
        brief.remove("request");
    }
    brief
}

/// A run's summary file, if it has one.
fn read_summary(dir: &Path, id: &str) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(dir.join(format!("{id}.summary"))).ok()?).ok()
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
            StrategyRef::Positions(list) => {
                let spec = with_params(list.spec().kind, Arc::new(list.info()), value)?;
                let plugin: Arc<dyn SignalFactory> = Arc::new(list);
                Ok((spec, Some(plugin)))
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
        let strategy = args.get("strategy").unwrap_or(&Value::Null);
        let (spec, mut plugin) = self.resolve(strategy)?;
        let name = spec.info.name.clone();
        let market = self.load_market(&market_args)?;
        if let Some(list) = strategy.get("positions") {
            // Given by when bars open, as an agent sees them, and heard by when they close.
            let opens: Vec<u64> = market.candles.iter().map(|c| c.ts).collect();
            let closes = market.bars.closes(&market.candles);
            let list = Positions::parse(list)?.at_closes(&opens, &closes)?;
            plugin = Some(Arc::new(list));
        }
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
        let bars: Vec<(u64, f64)> = request.candles.iter().map(|c| (c.ts, c.close)).collect();
        let report = backtest(request)?;
        let mut summary = summary_of(&report, from_index, &name, &bars);
        if let Some(label) = args.get("label") {
            summary["label"] = label.clone();
        }
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
                let brief = json!({
                    "run_id": run_id, "created": now_ms(), "summary": summary, "request": args,
                });
                fs::write(dir.join(format!("{run_id}.summary")), brief.to_string())
            })
            .map_err(|e| format!("couldn't save the run: {e}"))?;
        // Whatever's asked next is most likely about this run.
        self.remember_run(&run_id, Arc::new(run));
        Ok(json!({"run_id": run_id, "summary": summary}))
    }

    /// Many backtests from one call, in parallel, answered as a table.
    pub(crate) fn run_backtests(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let items = args
            .get("runs")
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or("`runs` should be a list of run_backtest's arguments")?;
        if items.len() > MAX_BACKTESTS {
            return Err(format!(
                "`runs`: that's {} backtests, more than the {MAX_BACKTESTS} allowed in a call",
                items.len()
            ));
        }
        let shared = args.get("defaults").and_then(Value::as_object);
        let calls: Vec<Value> = items
            .iter()
            .map(|item| {
                let own = item
                    .as_object()
                    .ok_or_else(|| format!("`runs`: {item} isn't an object"))?;
                let mut call = shared.cloned().unwrap_or_default();
                call.extend(own.clone());
                Ok(Value::Object(call))
            })
            .collect::<Result<_, String>>()?;
        // One at a time if any of them is a strategy in a process of its own.
        let workers = calls
            .iter()
            .filter_map(|call| self.resolve(call.get("strategy")?).ok())
            .map(|(spec, _)| self.workers_for(&spec))
            .min()
            .unwrap_or(1);
        job.total.store(calls.len(), Ordering::Relaxed);
        let all = tools();
        let tool = all
            .iter()
            .find(|tool| tool.name == "run_backtest")
            .ok_or("run_backtest is missing")?;
        let results = pool(
            &calls,
            workers,
            &job.stop,
            &|| {
                job.done.fetch_add(1, Ordering::Relaxed);
            },
            &|call| {
                let result = check_args(tool, call).and_then(|()| self.run_backtest(call));
                // Each is logged as a backtest of its own, so the log shows what was really tried.
                self.log_call("run_backtest", call, &result);
                result
            },
        );
        let (mut succeeded, mut failed, mut not_run) = (Vec::new(), Vec::new(), 0);
        for (index, result) in results.into_iter().enumerate() {
            match result {
                Some(Ok(run)) => succeeded.push((index, run)),
                Some(Err(error)) => failed.push((index, error)),
                None => not_run += 1,
            }
        }
        let net = |run: &Value| run["summary"]["net_return"].as_f64().unwrap_or(f64::MIN);
        succeeded.sort_by(|(_, a), (_, b)| net(b).total_cmp(&net(a)));
        let total = calls.len();
        let (done, errors) = (succeeded.len(), failed.len());
        let first_error = failed
            .first()
            .map(|(index, error)| json!({"index": index, "error": error}));
        let mut rows: Vec<Value> = succeeded
            .iter()
            .map(|(index, run)| {
                let summary = &run["summary"];
                json!([
                    index,
                    run["run_id"],
                    summary["net_return"],
                    summary["max_drawdown"],
                    summary["trades"],
                    summary["win_rate"],
                    summary["fees"],
                    summary["final_equity"],
                    summary["buy_and_hold_return"],
                    summary["sharpe"],
                    summary["profit_factor"],
                    summary["exposure"],
                    summary["average_trade"],
                    summary["warnings"].as_array().map_or(0, Vec::len),
                    null,
                ])
            })
            .collect();
        rows.extend(failed.iter().map(|(index, error)| {
            let mut row = vec![Value::Null; BATCH_COLUMNS.len()];
            row[0] = json!(index);
            row[BATCH_COLUMNS.len() - 1] = json!(error);
            Value::Array(row)
        }));
        rows.truncate(limit_of(args, DEFAULT_ROWS));
        Ok(json!({
            "columns": BATCH_COLUMNS,
            "rows": rows,
            "total": total,
            "succeeded": done,
            "failed": errors,
            "not_run": not_run,
            "first_error": first_error,
        }))
    }

    fn remember_run(&self, id: &str, run: Arc<Value>) {
        if let Ok(mut recent) = self.recent_runs.lock() {
            recent.push_front((id.to_owned(), run));
            recent.truncate(RECENT_RUNS);
        }
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
        self.remember_run(id, Arc::clone(&run));
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
        let trades = round_trips(&report);
        Ok(page_of(
            &TRADE_COLUMNS,
            trades.len(),
            args,
            DEFAULT_ROWS,
            |i| trade_row(i + 1, &trades[i], report.starting_balance),
        ))
    }

    pub(crate) fn get_fills(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let empty = Vec::new();
        let fills = run["fills"].as_array().unwrap_or(&empty);
        Ok(page_of(
            &["ts", "bar", "side", "price", "size", "fee"],
            fills.len(),
            args,
            DEFAULT_ROWS,
            |i| fills[i].clone(),
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
            DEFAULT_ROWS,
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
        Ok(page_of(
            &["ts", "return"],
            rows.len(),
            args,
            DEFAULT_ROWS,
            |i| rows[i].clone(),
        ))
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
        // A run's id says when it was made, so only the page's files need reading.
        let mut ids: Vec<(u64, u64, String)> = fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let id = name.to_str()?.strip_suffix(".summary")?;
                let mut parts = id.split('-').skip(1);
                let made = parts.next()?.parse().ok()?;
                let number = parts.nth(1)?.parse().ok()?;
                Some((made, number, id.to_owned()))
            })
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        // Searching means reading them all, which only asking for it costs.
        let label = args.get("label").and_then(Value::as_str);
        let sort = args.get("sort").and_then(Value::as_str);
        if label.is_some() || sort.is_some() {
            let mut briefs: Vec<Value> = ids
                .iter()
                .filter_map(|(_, _, id)| read_summary(&dir, id))
                .filter(|brief| label.is_none_or(|label| brief["summary"]["label"] == label))
                .collect();
            if let Some(sort) = sort {
                let ascending = args
                    .get("ascending")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let figure = |brief: &Value| brief["summary"][sort].as_f64();
                // Those without the figure go last, whichever way it's sorted.
                briefs.sort_by(|a, b| match (figure(a), figure(b)) {
                    (Some(a), Some(b)) if ascending => a.total_cmp(&b),
                    (Some(a), Some(b)) => b.total_cmp(&a),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                });
            }
            return Ok(page_of(
                &["run_id", "created", "summary"],
                briefs.len(),
                args,
                LISTED_RUNS,
                |i| without_request(briefs[i].clone()),
            ));
        }
        Ok(page_of(
            &["run_id", "created", "summary"],
            ids.len(),
            args,
            LISTED_RUNS,
            |i| {
                let id = &ids[i].2;
                without_request(read_summary(&dir, id).unwrap_or_else(|| json!({"run_id": id})))
            },
        ))
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
                check_id(id)?;
                let brief = read_summary(&self.paths.lab().join("runs"), id)
                    .filter(|brief| brief.get("request").is_some());
                // Saved before summaries held the request.
                let (summary, request) = match brief {
                    Some(brief) => (brief["summary"].clone(), brief["request"].clone()),
                    None => {
                        let run = self.load_run(id)?;
                        (run["summary"].clone(), run["request"].clone())
                    }
                };
                Ok(json!({"run_id": id, "summary": summary, "request": request}))
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
        let market: Vec<(u64, f64)> = candles.iter().map(|c| (c.ts, c.close)).collect();
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
        // What the market itself did over each block.
        let held_over = |from: u64, to: u64| {
            let first = market.partition_point(|bar| bar.0 < from);
            let last = market.partition_point(|bar| bar.0 <= to).saturating_sub(1);
            market[last].1 / market[first.min(last)].1 - 1.0
        };
        let mut held: Vec<f64> = summary
            .blocks
            .iter()
            .map(|block| held_over(block.from, block.to))
            .collect();
        let rows: Vec<Value> = summary
            .blocks
            .iter()
            .zip(&summary.trades)
            .zip(&held)
            .map(|((block, trades), held)| {
                let mut row = json!({
                    "from": time_text(block.from),
                    "to": time_text(block.to),
                    "buy_and_hold_return": held,
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
        held.sort_by(f64::total_cmp);
        let median_held = held.get(held.len() / 2).copied();
        Ok(json!({
            "profitable": summary.profitable,
            "median": summary.median,
            "median_buy_and_hold": median_held,
            "best": summary.best,
            "worst": summary.worst,
            "worst_dates": [time_text(summary.worst_dates.0), time_text(summary.worst_dates.1)],
            "blocks": rows,
        }))
    }

    /// Picks values on one stretch, tests them on the next, and slides along. Every unseen stretch
    /// starts from the same capital, as there's no compounding, so their returns add up.
    pub(crate) fn run_walk_forward(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let (mut request, _, _) = self.prepare(args)?;
        let workers = self.workers_for(&request.strategy);
        let chosen = optimize_choice(args, false)?;
        if chosen.score == ScoreBy::RandomBlocks {
            return Err("`score`: walk-forward takes backtest or monte-carlo".into());
        }
        let grid = Grid::build(&chosen, &request.strategy.info.params)?;
        let candles = std::mem::take(&mut request.candles);
        request.ticks = None;
        request.fill_bars = None;
        let from = candles.partition_point(|c| c.ts < request.trade_from);
        let windows = number(args, "windows", WALK_WINDOWS).max(1) as usize;
        let ratio = args
            .get("train_ratio")
            .and_then(Value::as_f64)
            .filter(|ratio| *ratio > 0.0)
            .unwrap_or(WALK_RATIO);
        let bars = candles.len().saturating_sub(from);
        let test = (bars as f64 / (windows as f64 + ratio)) as usize;
        let train = (test as f64 * ratio) as usize;
        if test < 2 || train < 2 {
            return Err(format!(
                "{bars} bars is too few for {windows} windows; widen the span or ask for fewer"
            ));
        }
        // The last unseen stretch ends on the last bar.
        let first = candles.len() - windows * test - train;
        job.total
            .store(windows * (grid.cells() + 1), Ordering::Relaxed);
        let warm_up = WARM_UP_BARS as usize;
        let mut finished = 0;
        let mut rows = Vec::with_capacity(windows);
        let (mut unseen_total, mut held_total) = (0.0, 0.0);
        let (mut trades, mut profitable, mut deepest) = (0, 0, 0.0_f64);
        for window in 0..windows {
            let train_start = first + window * test;
            let train_end = train_start + train;
            let test_end = train_end + test;
            let warm_from = train_start.saturating_sub(warm_up);
            let mut picking = request.clone();
            picking.trade_from = candles[train_start].ts;
            let batch = OptimizeJob {
                request: picking,
                candles: candles[warm_from..train_end].to_vec(),
                grid: grid.clone(),
                score: chosen.score,
                split: None,
                blocks: Vec::new(),
                warm_up,
                runs: number(args, "runs", 1_000) as u32,
                ruin_at: args
                    .get("ruin_percent")
                    .and_then(Value::as_f64)
                    .unwrap_or(50.0)
                    / 100.0,
                seed: number(args, "seed", 0),
                workers,
                stop: Arc::clone(&job.stop),
            };
            let found = batch.run(&|done| job.done.store(finished + done, Ordering::Relaxed))?;
            finished += grid.cells();
            let scores: Vec<Option<f64>> = found.outcomes.iter().map(Outcome::score).collect();
            let times = json!({
                "train_from": time_text(candles[train_start].ts),
                "test_from": time_text(candles[train_end].ts),
                "test_to": time_text(candles[test_end - 1].ts),
            });
            let mut row = times;
            let Some(&(cell, _)) = rank(&scores, grid.width()).first() else {
                row["status"] = json!("nothing-to-pick");
                rows.push(row);
                continue;
            };
            let mut trying = request.clone();
            for (index, value) in grid.values_of(cell) {
                trying.strategy.set_param(index, value)?;
            }
            trying.candles = candles[train_end.saturating_sub(warm_up)..test_end].to_vec();
            trying.trade_from = candles[train_end].ts;
            let report = backtest(trying)?;
            finished += 1;
            job.done.store(finished, Ordering::Relaxed);
            let capital = report.starting_balance;
            let unseen = report.equity.last().copied().unwrap_or(capital) / capital - 1.0;
            let made = round_trips(&report).len();
            // From the close before the stretch's first order could fill, as a strategy's does.
            let held = candles[test_end - 1].close / candles[train_end].close - 1.0;
            held_total += held;
            row["buy_and_hold_return"] = json!(held);
            unseen_total += unseen;
            trades += made;
            profitable += usize::from(unseen > 0.0);
            deepest = deepest.max(report.max_drawdown.abs());
            row["values"] = names_of(&grid, cell);
            row["picked_score"] = json!(scores[cell]);
            row["unseen_return"] = json!(unseen);
            row["fell"] = json!(report.max_drawdown.abs());
            row["trades"] = json!(made);
            row["status"] = json!("tested");
            rows.push(row);
        }
        Ok(json!({
            "windows": rows,
            "unseen_return": unseen_total,
            "buy_and_hold_return": held_total,
            "profitable_windows": profitable,
            "deepest_fall": deepest,
            "trades": trades,
            "train_bars": train,
            "test_bars": test,
        }))
    }

    pub(crate) fn run_optimize(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let (mut request, _, _) = self.prepare(args)?;
        let workers = self.workers_for(&request.strategy);
        let hold_out = args
            .get("hold_out")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let chosen = optimize_choice(args, hold_out)?;
        let score = chosen.score;
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
        let names_of = |cell: usize| names_of(&found.grid, cell);
        let mut rows: Vec<Value> = found
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
        // Best first, and the list cut short: a grid can have thousands.
        let score_of = |row: &Value| row["score"].as_f64().unwrap_or(f64::MIN);
        rows.sort_by(|a, b| score_of(b).total_cmp(&score_of(a)));
        let values = rows.len();
        rows.truncate(limit_of(args, LISTED_VALUES));
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
            "total": values,
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
