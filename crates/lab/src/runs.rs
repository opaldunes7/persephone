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
use persephone_engine::{BacktestReport, BacktestRequest, Fill, LimitOutcome};
use persephone_research::{
    WARM_UP_BARS,
    calendar::{Calendar, days_in_month, weeks},
    figures,
    market::{MAX_FINE_BARS, MarketBars, fine_bars},
    optimize::{Grid, OptimizeJob, Outcome, split_at},
    positions::{ONE_TRADE, Positions, RandomEntriesJob},
    results::{StrategyRef, with_params},
    robustness::{
        BlockJob, MonteCarlo, RoundTrip, backtest, closed_trades, monte_carlo,
        monte_carlo_compounding, pick_blocks, pnl_by_trade, pool, round_trips,
    },
    settings::{BlockLength, FillTiming, OptimizeSettings, Precision, ScoreBy, Settings, Variable},
    walk_forward::{RATIO, WINDOWS, WalkForward, WalkForwardJob, stretches},
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
const BATCH_COLUMNS: [&str; 35] = [
    "index",
    "run_id",
    "label",
    "fills",
    "net_return",
    "max_drawdown",
    "trades",
    "win_rate",
    "fees",
    "final_equity",
    "buy_and_hold_return",
    "excess_return",
    "sharpe",
    "profit_factor",
    "exposure",
    "average_trade",
    "by_year",
    "gross_return",
    "total_costs_paid",
    "break_even_cost_per_fill",
    "maker_fees",
    "taker_fees",
    "slippage_paid",
    "spread_paid",
    "range_slippage_paid",
    "impact_paid",
    "funding_paid",
    "borrow_paid",
    "limit_orders",
    "limits_filled",
    "fill_rate",
    "orders_worked",
    "unfilled_at_end",
    "warnings",
    "error",
];
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
    let funding = settings.funding(market.funding);
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
        funding,
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

pub(crate) fn settings_of(args: &Value) -> Result<Settings, String> {
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
    funding: &[(u64, f64)],
) -> Value {
    let trades = round_trips(report);
    let profit: f64 = trades.iter().map(|t| t.profit).sum();
    let gains: f64 = trades.iter().map(|t| t.profit.max(0.0)).sum();
    let losses: f64 = trades.iter().map(|t| (-t.profit).max(0.0)).sum();
    let winners = trades.iter().filter(|t| t.profit > 0.0).count();
    let (by_hour, by_weekday) = figures::by_time(&trades);
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
        "buy_and_hold_return": figures::buy_and_hold(bars, from_index),
        "benchmark": figures::benchmark(report.starting_balance, bars, from_index, funding).map(|held| {
            json!({
                "return": held.return_,
                "return_after_funding": held.return_after_funding,
                "sharpe": held.sharpe,
                "max_drawdown": held.max_drawdown,
            })
        }),
        "sharpe": figures::sharpe(report, from_index, bars),
        "profit_factor": (losses > 0.0).then(|| gains / losses),
        "exposure": figures::exposure(report, from_index),
        "average_trade": (!trades.is_empty()).then(|| profit / trades.len() as f64),
        "by_year": figures::by_year(report, from_index, bars),
        "by_year_compounded": figures::by_year_compounded(report, from_index, bars),
        "beta": figures::beta(report, from_index, bars),
        "by_hour": by_hour,
        "by_weekday": by_weekday,
        "excess_return": figures::benchmark(report.starting_balance, bars, from_index, funding)
            .map(|held| last / report.starting_balance - 1.0 - held.return_after_funding),
        "warnings": warnings_of(report),
    })
}

/// What needs saying about a run: its own warnings, and if it made no trades, why that may be.
fn warnings_of(report: &BacktestReport) -> Vec<String> {
    let mut warnings: Vec<String> = report
        .notes
        .iter()
        .filter(|note| note.warning)
        .map(|note| note.text.clone())
        .collect();
    if report.fills.is_empty() {
        warnings.push(format!(
            "It made no trades. A look-back longer than the `warm_up_bars` ({WARM_UP_BARS} unless set) warmed up on before the span isn't ready until that far in, and a rule on it is false until then."
        ));
    }
    warnings
}

fn limit_outcome(outcome: LimitOutcome) -> &'static str {
    match outcome {
        LimitOutcome::Filled => "filled",
        LimitOutcome::Lapsed => "lapsed",
        LimitOutcome::Replaced => "replaced",
        LimitOutcome::Cancelled => "cancelled",
        LimitOutcome::Refused => "refused",
        LimitOutcome::Open => "open",
    }
}

/// Whether a call asks for every line a fan draws, rather than the few that sum it up.
fn wants_lines(args: &Value) -> bool {
    args.get("include_lines")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// An equity line from a fan, thinned to about 100 points to the cent: a fan's lines are a lot for an
/// agent to read, and this tells it what the shape needs.
fn thin_line(line: &[f64]) -> Vec<f64> {
    const POINTS: usize = 100;
    let Some(last) = line.len().checked_sub(1) else {
        return Vec::new();
    };
    let stride = line.len().div_ceil(POINTS).max(1);
    (0..line.len())
        .step_by(stride)
        .chain((!last.is_multiple_of(stride)).then_some(last))
        .map(|at| (line[at] * 100.0).round() / 100.0)
        .collect()
}

/// Walk-forward's chart as the app's panel draws it, thinned as a full one would cost an agent about
/// 10,000 tokens each call to tell it no more.
fn walk_chart(found: &WalkForward) -> Value {
    const POINTS: usize = 200;
    let count = found.unseen_line.len();
    let stride = count.div_ceil(POINTS).max(1);
    let thin = |line: &[f64]| -> Vec<f64> {
        (0..count)
            .step_by(stride)
            .chain((!(count - 1).is_multiple_of(stride)).then_some(count - 1))
            .map(|at| (line[at] * 1e4).round() / 1e4)
            .collect()
    };
    json!({
        "unseen_percent": thin(&found.unseen_line),
        "market_percent": thin(&found.market_line),
        "window_starts": found.starts.iter().map(|&(_, at)| at / stride).collect::<Vec<_>>(),
    })
}

/// A spread as the app's panel draws it: the range, how many lie in each equal step across it, and
/// where the backtest's own figure lies.
pub(crate) fn histogram_json(values: &[f64], actual: f64) -> Value {
    let found = figures::Histogram::of(values, figures::HISTOGRAM_BINS, actual);
    json!({"from": found.lo, "to": found.hi, "counts": found.counts, "actual": actual})
}

/// A walk-forward's answer, for one market or a portfolio.
pub(crate) fn walk_answer(found: &WalkForward, variants_tried: u64) -> Value {
    let rows: Vec<Value> = found
        .windows
        .iter()
        .map(|window| {
            let mut row = json!({
                "train_from": time_text(window.train_from),
                "test_from": time_text(window.test_from),
                "test_to": time_text(window.test_to),
                "status": "nothing-to-pick",
            });
            if let Some(picked) = &window.picked {
                let values: Map<String, Value> = picked
                    .values
                    .iter()
                    .map(|(name, value)| (name.clone(), json!(value)))
                    .collect();
                row["label"] = json!(picked.label);
                row["buy_and_hold_return"] = json!(picked.buy_and_hold_return);
                row["values"] = Value::Object(values);
                row["picked_score"] = json!(picked.score);
                row["unseen_return"] = json!(picked.unseen_return);
                row["fell"] = json!(picked.fell);
                row["trades"] = json!(picked.trades);
                row["picked_sharpe"] = json!(picked.picked_sharpe);
                row["deflated_sharpe"] = json!(picked.deflated_sharpe);
                row["status"] = json!("tested");
            }
            row
        })
        .collect();
    let (unseen_compounded, _) = found.compounded();
    json!({
        // The fair comparison first: what an account would have made, against the market held.
        "headline": {
            "unseen_compounded": unseen_compounded,
            "buy_and_hold_whole": found.buy_and_hold_whole,
            "beat_market_by": unseen_compounded - found.buy_and_hold_whole,
            "unseen_sharpe": found.unseen_sharpe,
        },
        "windows": rows,
        "unseen_return": found.unseen_return,
        "unseen_compounded": found.compounded().0,
        "buy_and_hold_compounded": found.compounded().1,
        "buy_and_hold_return": found.buy_and_hold_return,
        "buy_and_hold_whole": found.buy_and_hold_whole,
        "unseen_sharpe": found.unseen_sharpe,
        "profitable_windows": found.profitable_windows,
        "deepest_fall": found.deepest_fall,
        "trades": found.trades,
        "train_bars": found.train_bars,
        "test_bars": found.test_bars,
        "variants_tried": variants_tried,
        "chart": walk_chart(found),
    })
}

/// A page whose rows are objects, so has no columns to name.
fn named_rows(mut page: Value) -> Value {
    if let Some(page) = page.as_object_mut() {
        page.remove("columns");
    }
    page
}

/// How many bars a logic tree needs before all its rules are ready. Nothing else says.
/// The best of a batch's Sharpe ratio, deflated for having been picked from all of them, worked out
/// from its saved run. `sharpes` are every run's.
pub(crate) fn batch_deflated_sharpe(
    picked: f64,
    best: &Value,
    sharpes: &[f64],
    trials: u64,
) -> Option<f64> {
    let from = best["from_index"].as_u64().unwrap_or(0) as usize;
    let times: Vec<u64> = best["times"]
        .as_array()?
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    let report = report_from(best);
    let daily = figures::run_daily_returns(&report, from, &times);
    figures::deflated_sharpe_of(picked, sharpes, trials, &daily)
}

/// How many variants the run being tested was picked from, as said, or 1.
pub(crate) fn variants_tried(args: &Value) -> u64 {
    args.get("variants_tried")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1)
}

/// Said when a logic tree looks back further than the bars each stretch of a test is warmed up on,
/// so its rules can't be true at the start of every stretch. A test of stretches can't say it per
/// stretch, so it's said once.
fn stretch_look_back_warning(
    args: &Value,
    request: &BacktestRequest,
    warm_up: usize,
) -> Option<String> {
    let look_back = look_back_of(
        args,
        request.strategy.own_params(),
        request.bars.timeframe().map_or(0, Timeframe::nanos),
    );
    (look_back > warm_up).then(|| {
        format!(
            "A look-back here needs about {look_back} bars, but each stretch is warmed up on only {warm_up} (`warm_up_bars`), so rules on it can't be true for about the first {} bars of each.",
            look_back - warm_up
        )
    })
}

pub(crate) fn look_back_of(args: &Value, params: &[f64], chart_bar: u64) -> usize {
    args["strategy"]
        .get("logic")
        .map_or(0, |_| match StrategyRef::parse(&args["strategy"]) {
            Ok(StrategyRef::Logic(logic)) => logic.look_back_on(params, chart_bar),
            _ => 0,
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
pub(crate) fn report_from(run: &Value) -> BacktestReport {
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
                maker: fill[6].as_bool().unwrap_or(false),
                range_slip: fill[7].as_f64().unwrap_or(0.0),
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
        limit_orders: run["summary"]["limit_orders"].as_u64().unwrap_or(0) as usize,
        limits_filled: run["summary"]["limits_filled"].as_u64().unwrap_or(0) as usize,
        decisions: Vec::new(),
        orders_worked: run["summary"]["orders_worked"].as_u64().unwrap_or(0) as usize,
        bars_to_fill_average: run["summary"]["bars_to_fill_average"]
            .as_f64()
            .unwrap_or(0.0),
        bars_to_fill_longest: run["summary"]["bars_to_fill_longest"].as_u64().unwrap_or(0) as usize,
        unfilled_at_end: run["summary"]["unfilled_at_end"].as_f64().unwrap_or(0.0),
        funding_paid: run["summary"]["funding_paid"].as_f64().unwrap_or(0.0),
        borrow_paid: run["summary"]["borrow_paid"].as_f64().unwrap_or(0.0),
        limit_log: Vec::new(),
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
    /// Whether a saved run is of several markets.
    pub(crate) fn is_portfolio_run(&self, id: &str) -> bool {
        check_id(id).is_ok()
            && read_summary(&self.paths.lab().join("runs"), id)
                .is_some_and(|brief| brief["summary"]["kind"] == "portfolio")
    }

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
    pub(crate) fn prepare(&self, args: &Value) -> Result<(BacktestRequest, usize, String), String> {
        let market_args = MarketArgs::parse(args)?;
        let settings = settings_of(args)?;
        if settings.fill_timing == FillTiming::NextOpen && settings.precision == Precision::High {
            return Err(
                "filling at the next open can't go with latency or high precision; pick one".into(),
            );
        }
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
        let look_back = look_back_of(
            args,
            request.strategy.own_params(),
            request.bars.timeframe().map_or(0, Timeframe::nanos),
        );
        let market_share = request.strategy.size_fraction() * request.sim.leverage;
        let funding = request.funding.as_deref().cloned().unwrap_or_default();
        let fills_on = if request.sim.next_open {
            "next_open"
        } else if request.ticks.is_some() {
            "ticks"
        } else if request.fill_bars.is_some() {
            "minute_bars"
        } else {
            "bars"
        };
        let sim = request.sim;
        let price_precision = request.instrument.price_precision;
        let wanted = args
            .get("friction")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let volumes: Vec<f64> = request.candles.iter().map(|c| c.volume).collect();
        let free = (wanted && figures::costs_something(&request))
            .then(|| figures::without_costs(&request));
        let report = backtest(request)?;
        let free = free.map(backtest).transpose()?;
        let mut summary = summary_of(&report, from_index, &name, &bars, &funding);
        if wanted {
            let gross = free.as_ref().unwrap_or(&report);
            let friction = figures::friction(&report, gross, &sim, price_precision, &volumes);
            summary["gross_return"] = json!(friction.gross_return);
            summary["total_costs_paid"] = json!(friction.costs_paid);
            summary["maker_fees"] = json!(friction.maker_fees);
            summary["taker_fees"] = json!(friction.taker_fees);
            summary["slippage_paid"] = json!(friction.costs.slippage);
            summary["spread_paid"] = json!(friction.costs.spread);
            summary["impact_paid"] = json!(friction.costs.impact);
            summary["cost_rounding"] = json!(friction.costs.rounding);
            summary["break_even_cost_per_fill"] = json!(friction.break_even_bp);
        } else {
            for figure in [
                "gross_return",
                "total_costs_paid",
                "break_even_cost_per_fill",
                "slippage_paid",
                "spread_paid",
                "impact_paid",
                "cost_rounding",
            ] {
                summary[figure] = Value::Null;
            }
        }
        let limits = figures::LimitSummary::of(&report);
        for warning in [
            figures::look_back_warning(look_back, from_index, times.len()),
            limits.warning(),
            settings_of(args)?.ignored_warning(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(warnings) = summary["warnings"].as_array_mut() {
                warnings.push(json!(warning));
            }
        }
        summary["limit_summary"] = json!((report.limit_orders > 0).then(|| json!({
            "filled": limits.filled,
            "missed": limits.missed,
            "barely_filled": limits.barely_filled,
            "filled_move_1": limits.filled_move_1,
            "missed_move_1": limits.missed_move_1,
            "filled_move_24": limits.filled_move_24,
            "missed_move_24": limits.missed_move_24,
        })));
        if let Some(label) = args.get("label") {
            summary["label"] = label.clone();
        }
        // Which bars these were, as a span with no end moves on as new bars close.
        summary["fills_on"] = json!(fills_on);
        summary["funding_paid"] = json!(report.funding_paid);
        summary["borrow_paid"] = json!(report.borrow_paid);
        summary["limit_orders"] = json!(report.limit_orders);
        summary["limits_filled"] = json!(report.limits_filled);
        summary["fill_rate"] = json!(
            (report.limit_orders > 0)
                .then(|| report.limits_filled as f64 / report.limit_orders as f64)
        );
        summary["range_slippage_paid"] = json!(
            report
                .fills
                .iter()
                .map(|fill| fill.qty * fill.range_slip)
                .sum::<f64>()
                + 0.0
        );
        summary["orders_worked"] = json!(report.orders_worked);
        summary["bars_to_fill_average"] = json!(report.bars_to_fill_average);
        summary["bars_to_fill_longest"] = json!(report.bars_to_fill_longest);
        summary["unfilled_at_end"] = json!(report.unfilled_at_end);
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
            "closes": bars.iter().map(|(_, close)| close).collect::<Vec<_>>(),
            "limit_log": report.limit_log.iter().map(|order| json!([
                order.placed,
                time_text(order.placed),
                if order.side == Side::Buy { "buy" } else { "sell" },
                order.price,
                order.qty,
                limit_outcome(order.outcome),
                order.ended,
                order.bars,
                order.reached_percent,
                order.move_1_percent,
                order.move_24_percent,
                order.filled,
            ])).collect::<Vec<_>>(),
            "market_share": market_share,
            "decisions": report.decisions.iter().map(|row| json!([
                row.ts, time_text(row.ts), row.wanted, row.held, row.target, row.order, row.why,
            ])).collect::<Vec<_>>(),
            "fills": report.fills.iter().map(|f| json!([
                f.ts, f.bar, if f.side == Side::Buy { "buy" } else { "sell" }, f.price, f.qty, f.fee, f.maker, f.range_slip,
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
    /// A run's holds at random times, many times over: what the same trading comes to with entries
    /// owing nothing to the strategy.
    pub(crate) fn run_random_entries(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let report = report_from(&run);
        let (request, from_index, _) = self.prepare(&run["request"])?;
        let draws = number(args, "runs", 200).clamp(1, MAX_BACKTESTS as u64) as usize;
        job.total.store(draws, Ordering::Relaxed);
        let found = RandomEntriesJob {
            report: Arc::new(report),
            workers: self.workers_for(&request.strategy),
            request,
            from_index,
            runs: draws,
            seed: number(args, "seed", 0),
            stop: Arc::clone(&job.stop),
        }
        .run(&|done| job.done.store(done, Ordering::Relaxed))?;
        Ok(json!({
            "runs": found.runs,
            "trades": found.trades,
            "actual_return": found.actual_return,
            "actual_fell": found.actual_fell,
            "share_matching_return": found.share_matching_return,
            "share_matching_fall": found.share_matching_fall,
            "variants_tried": variants_tried(args),
            "share_matching_adjusted": figures::adjusted_for_variants(found.share_matching_return, variants_tried(args)),
            "warning": (found.trades < 2).then_some(ONE_TRADE),
            "median": found.median,
            "low": found.low,
            "high": found.high,
            "return_chart": histogram_json(&found.returns, found.actual_return),
            "fall_chart": histogram_json(&found.falls, found.actual_fell),
        }))
    }

    pub(crate) fn run_backtests(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        // A path, as a long list costs an agent a lot to write out in a call.
        let from_file = match args.get("runs").and_then(Value::as_str) {
            Some(path) => {
                let text = fs::read_to_string(path)
                    .map_err(|e| format!("`runs`: couldn't read {path}: {e}"))?;
                Some(
                    serde_json::from_str::<Value>(&text)
                        .map_err(|e| format!("`runs`: {path} isn't JSON: {e}"))?,
                )
            }
            None => None,
        };
        let items = from_file
            .as_ref()
            .or_else(|| args.get("runs"))
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or("`runs` should be a list of run_backtest's arguments, or a JSON file's path")?;
        if items.len() > MAX_BACKTESTS {
            return Err(format!(
                "`runs`: that's {} backtests, more than the {MAX_BACKTESTS} allowed in a call",
                items.len()
            ));
        }
        // Known now, so a call asking straight away hears how many, not 0 while they're read.
        job.total.store(items.len(), Ordering::Relaxed);
        let shared = args.get("defaults").and_then(Value::as_object);
        let calls: Vec<Value> = items
            .iter()
            .map(|item| {
                let own = item
                    .as_object()
                    .ok_or_else(|| format!("`runs`: {item} isn't an object"))?;
                Ok(crate::tools::merge_call(shared, own))
            })
            .collect::<Result<_, String>>()?;
        let specs: Vec<Option<StrategySpec>> = calls
            .iter()
            .map(|call| Some(self.resolve(call.get("strategy")?).ok()?.0))
            .collect();
        let warnings: Vec<String> = size_mismatch(&specs).into_iter().collect();
        // One at a time if any of them is a strategy in a process of its own.
        let workers = specs
            .iter()
            .flatten()
            .map(|spec| self.workers_for(spec))
            .min()
            .unwrap_or(1);
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
                    summary["label"],
                    summary["fills"],
                    summary["net_return"],
                    summary["max_drawdown"],
                    summary["trades"],
                    summary["win_rate"],
                    summary["fees"],
                    summary["final_equity"],
                    summary["buy_and_hold_return"],
                    summary["excess_return"],
                    summary["sharpe"],
                    summary["profit_factor"],
                    summary["exposure"],
                    summary["average_trade"],
                    summary["by_year"],
                    summary["gross_return"],
                    summary["total_costs_paid"],
                    summary["break_even_cost_per_fill"],
                    summary["maker_fees"],
                    summary["taker_fees"],
                    summary["slippage_paid"],
                    summary["spread_paid"],
                    summary["range_slippage_paid"],
                    summary["impact_paid"],
                    summary["funding_paid"],
                    summary["borrow_paid"],
                    summary["limit_orders"],
                    summary["limits_filled"],
                    summary["fill_rate"],
                    summary["orders_worked"],
                    summary["unfilled_at_end"],
                    summary["warnings"],
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
        // The top row, made fair for being the best of all that ran, and of all tried before.
        let sharpes: Vec<f64> = succeeded
            .iter()
            .filter_map(|(_, run)| run["summary"]["sharpe"].as_f64())
            .collect();
        let trials = variants_tried(args).max(done as u64);
        let deflated = succeeded.first().and_then(|(_, best)| {
            let picked = best["summary"]["sharpe"].as_f64()?;
            let saved = self.load_run(best["run_id"].as_str()?).ok()?;
            batch_deflated_sharpe(picked, &saved, &sharpes, trials)
        });
        Ok(json!({
            "columns": BATCH_COLUMNS.as_slice(),
            "rows": rows,
            "total": total,
            "succeeded": done,
            "failed": errors,
            "not_run": not_run,
            "first_error": first_error,
            "variants_tried": trials,
            "deflated_sharpe": deflated,
            "warnings": warnings,
        }))
    }

    pub(crate) fn remember_run(&self, id: &str, run: Arc<Value>) {
        if let Ok(mut recent) = self.recent_runs.lock() {
            recent.push_front((id.to_owned(), run));
            recent.truncate(RECENT_RUNS);
        }
    }

    /// A saved run whole, for tests to look inside.
    #[doc(hidden)]
    pub fn load_run_for_test(&self, id: &str) -> Result<Arc<Value>, String> {
        self.load_run(id)
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
        for folder in ["runs", "jobs", "answers"] {
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
        let report = report_from(&run);
        Ok(json!({
            "run_id": run["run_id"],
            "request": run["request"],
            "summary": run["summary"],
            "starting_balance": run["starting_balance"],
            "max_drawdown": run["max_drawdown"].as_f64().map(f64::abs),
            "fees": run["fees"],
            "total_orders": run["total_orders"],
            "total_positions": run["total_positions"],
            "pnl": map_of(&pnl_by_trade(&report)),
            "ends_with_open_trade": round_trips(&report).last().is_some_and(|t| t.open),
            "closed_positions_pnl": run["pnl"],
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

    /// Every limit order a run sent, filled or not, with what the market then did.
    pub(crate) fn get_limit_orders(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let empty = Vec::new();
        let wanted = args.get("outcome").and_then(Value::as_str);
        let rows: Vec<&Value> = run["limit_log"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter(|row| wanted.is_none_or(|outcome| row[5] == outcome))
            .collect();
        Ok(page_of(
            &[
                "placed_ts",
                "placed",
                "side",
                "price",
                "size",
                "outcome",
                "ended_ts",
                "bars",
                "reached_percent",
                "move_1_percent",
                "move_24_percent",
                "filled",
            ],
            rows.len(),
            args,
            DEFAULT_ROWS,
            |i| rows[i].clone(),
        ))
    }

    pub(crate) fn get_decisions(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let empty = Vec::new();
        let all = run["decisions"].as_array().unwrap_or(&empty);
        if all.is_empty() {
            return Err(
                "That run kept no decisions. Run it with `decision_log: true` in `settings`."
                    .into(),
            );
        }
        let wanted = args.get("why").and_then(Value::as_str);
        let wanted_only = args
            .get("wanted_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let asked: Vec<&Value> = all
            .iter()
            .filter(|row| !wanted_only || (!row[2].is_null() && row[6] != "warming up"))
            .collect();
        let rows: Vec<&Value> = asked
            .iter()
            .copied()
            .filter(|row| wanted.is_none_or(|why| row[6] == why))
            .collect();
        // Over what `wanted_only` leaves, but every reason, so `why` can pick among them.
        let mut reasons = BTreeMap::new();
        for row in asked {
            *reasons
                .entry(row[6].as_str().unwrap_or_default().to_owned())
                .or_insert(0_u64) += 1;
        }
        let mut page = page_of(
            &["ts", "time", "wanted", "held", "target", "order", "why"],
            rows.len(),
            args,
            DEFAULT_ROWS,
            |at| rows[at].clone(),
        );
        page["reasons"] = json!(reasons);
        Ok(page)
    }

    pub(crate) fn get_fills(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let empty = Vec::new();
        let fills = run["fills"].as_array().unwrap_or(&empty);
        Ok(page_of(
            &[
                "ts",
                "bar",
                "side",
                "price",
                "size",
                "fee",
                "maker",
                "range_slip",
            ],
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
        let closes = run["closes"].as_array().unwrap_or(&empty);
        let share = run["market_share"].as_f64();
        let first = closes.get(from).and_then(Value::as_f64);
        let capital = run["starting_balance"].as_f64().unwrap_or(0.0);
        Ok(page_of(
            &["ts", "time", "equity", "market"],
            total,
            args,
            DEFAULT_ROWS,
            |i| {
                let at = times[from + i].as_u64().unwrap_or(0);
                // Runs saved before this have no closes to work it out from.
                let market = share
                    .zip(first)
                    .zip(closes.get(from + i).and_then(Value::as_f64));
                let market = market.map(|((share, first), close)| {
                    figures::market_equity(capital, share, first, close)
                });
                json!([at, time_text(at), equity[from + i], market])
            },
        ))
    }

    pub(crate) fn get_returns(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_run(text(args, "run_id")?)?;
        let empty = Vec::new();
        // Warm-up days aren't part of the run, and would only be zeros.
        let first = run["times"]
            .get(run["from_index"].as_u64().unwrap_or(0) as usize)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let rows: Vec<&Value> = run["returns_series"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter(|row| row[0].as_u64().is_none_or(|ts| ts >= first))
            .collect();
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
            return Ok(named_rows(page_of(
                &[],
                briefs.len(),
                args,
                LISTED_RUNS,
                |i| without_request(briefs[i].clone()),
            )));
        }
        Ok(named_rows(page_of(
            &[],
            ids.len(),
            args,
            LISTED_RUNS,
            |i| {
                let id = &ids[i].2;
                without_request(read_summary(&dir, id).unwrap_or_else(|| json!({"run_id": id})))
            },
        )))
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
        // As the app does: `stock` being off turns compounding off with the rest.
        let compound = run["request"]
            .get("settings")
            .map_or(Ok(Settings::default()), read_settings)?
            .sim()
            .compound;
        let draw = if compound {
            monte_carlo_compounding
        } else {
            monte_carlo
        };
        let found = draw(
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
        let warm_up = settings_of(args)?.warm_up_bars as usize;
        let look_back = stretch_look_back_warning(args, &request, warm_up);
        let batch = BlockJob {
            request,
            candles,
            blocks,
            warm_up,
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
        let mut rows: Vec<Value> = summary
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
        let total = rows.len();
        let warning = summary
            .blocks
            .iter()
            .all(|block| block.fills == 0)
            .then(|| {
                format!(
                    "No block made a trade. A look-back longer than the {warm_up} bars warmed up on before each block is never ready in time."
                )
            })
            .or(look_back);
        rows.truncate(limit_of(args, LISTED_VALUES));
        let mut chart = json!({
            "bars_between_points": summary.stride,
            "median_line": thin_line(&summary.median_path),
            "best_line": summary.paths.get(summary.best_line).map(|line| thin_line(line)),
            "worst_line": summary.paths.get(summary.worst_line).map(|line| thin_line(line)),
        });
        if wants_lines(args) {
            chart["lines"] = json!(
                summary
                    .paths
                    .iter()
                    .map(|line| thin_line(line))
                    .collect::<Vec<_>>()
            );
        }
        Ok(json!({
            "chart": chart,
            "profitable": summary.profitable,
            "median": summary.median,
            "median_buy_and_hold": median_held,
            "best": summary.best,
            "worst": summary.worst,
            "worst_dates": [time_text(summary.worst_dates.0), time_text(summary.worst_dates.1)],
            "blocks": rows,
            "total": total,
            "warning": warning,
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
        let windows = number(args, "windows", WINDOWS as u64).max(1) as usize;
        let ratio = args
            .get("train_ratio")
            .and_then(Value::as_f64)
            .filter(|ratio| *ratio > 0.0)
            .unwrap_or(settings_of(args).map_or(RATIO, |settings| settings.walk_ratio));
        stretches(candles.len().saturating_sub(from), windows, ratio)?;
        let warm_up = settings_of(args)?.warm_up_bars as usize;
        let warning = stretch_look_back_warning(args, &request, warm_up);
        let trials = variants_tried(args).max(grid.cells() as u64);
        let batch = WalkForwardJob {
            request,
            candles,
            from,
            grid,
            score: chosen.score,
            windows,
            ratio,
            warm_up,
            runs: number(args, "runs", 1_000) as u32,
            ruin_at: args
                .get("ruin_percent")
                .and_then(Value::as_f64)
                .unwrap_or(50.0)
                / 100.0,
            seed: number(args, "seed", 0),
            trials,
            workers,
            stop: Arc::clone(&job.stop),
        };
        job.total.store(batch.total(), Ordering::Relaxed);
        let found = batch.run(&|done| job.done.store(done, Ordering::Relaxed))?;
        let mut answer = walk_answer(&found, trials);
        answer["warning"] = json!(warning);
        Ok(answer)
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
        let split = hold_out.then(|| {
            split_at(
                from,
                candles.len(),
                settings_of(args).map_or(0.7, |s| s.hold_out_percent / 100.0),
            )
        });
        let held_out_market = split.and_then(|split| {
            let first = candles.get(split)?.close;
            let last = candles.last()?.close;
            Some(last / first - 1.0)
        });
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
        let warning = figures::look_back_warning(
            look_back_of(
                args,
                request.strategy.own_params(),
                request.bars.timeframe().map_or(0, Timeframe::nanos),
            ),
            from,
            candles.len(),
        );
        let batch = OptimizeJob {
            request,
            candles,
            grid,
            score,
            split,
            blocks,
            warm_up: settings_of(args)?.warm_up_bars as usize,
            runs: number(args, "runs", 1_000) as u32,
            ruin_at: args
                .get("ruin_percent")
                .and_then(Value::as_f64)
                .unwrap_or(50.0)
                / 100.0,
            seed,
            trials: variants_tried(args),
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
                json!({"values": names_of(cell), "score": score, "sharpe": found.sharpes[cell], "status": status, "what": what})
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
                    "sharpe": found.sharpes[best.cell],
                    "with_neighbours": best.plateau,
                    "unseen_return": best.unseen,
                })
            })
            .collect();
        let scores: Vec<Option<f64>> = found
            .outcomes
            .iter()
            .map(|outcome| outcome.score().map(|score| (score * 1e6).round() / 1e6))
            .collect();
        let axis = |axis: &persephone_research::optimize::Axis| json!({"name": axis.name, "values": axis.values});
        Ok(json!({
            "chart": {
                "across": axis(&found.grid.first),
                "down": found.grid.second.as_ref().map(axis),
                "scores": scores,
            },
            "values": rows,
            "total": values,
            "top": top,
            "on_edge": found.on_edge,
            "held_out": found.held_out,
            "held_out_buy_and_hold": held_out_market,
            "backtests": total,
            "variants_tried": variants_tried(args).max(found.grid.cells() as u64),
            "deflated_sharpe": found.deflated_sharpe,
            "warning": warning,
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
    let mut chart = json!({
        "trades_between_points": found.stride,
        "median_line": thin_line(&found.median_path),
        "backtest_line": thin_line(&found.actual),
        "best_line": found.paths.get(found.best).map(|line| thin_line(line)),
        "worst_line": found.paths.get(found.worst).map(|line| thin_line(line)),
        "middle_line": found.paths.get(found.middle).map(|line| thin_line(line)),
    });
    if wants_lines(args) {
        chart["lines"] = json!(
            found
                .paths
                .iter()
                .map(|line| thin_line(line))
                .collect::<Vec<_>>()
        );
    }
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
        "chart": chart,
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

/// A batch whose runs trade different sizes, said plainly: their returns aren't comparable.
fn size_mismatch(specs: &[Option<StrategySpec>]) -> Option<String> {
    let mut sizes: Vec<(f64, Vec<usize>)> = Vec::new();
    for (index, spec) in specs.iter().enumerate() {
        let Some(spec) = spec else { continue };
        let size = spec.size_fraction() * 100.0;
        match sizes
            .iter_mut()
            .find(|(known, _)| (known - size).abs() < 1e-9)
        {
            Some((_, runs)) => runs.push(index),
            None => sizes.push((size, vec![index])),
        }
    }
    if sizes.len() < 2 {
        return None;
    }
    let groups: Vec<String> = sizes
        .iter()
        .map(|(size, runs)| {
            let shown: Vec<String> = runs.iter().take(8).map(ToString::to_string).collect();
            let more = if runs.len() > 8 { ", ..." } else { "" };
            format!("{size}% (runs {}{more})", shown.join(", "))
        })
        .collect();
    Some(format!(
        "These runs don't all trade the same Size %: {}. Their returns aren't comparable; set \
         `Size %` in `defaults.strategy.params` (a run with a strategy of its own keeps it unless \
         it sets its own).",
        groups.join("; ")
    ))
}
